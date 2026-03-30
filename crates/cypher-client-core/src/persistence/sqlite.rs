//! SQLite-backed message store with zstd compression and AES-256-GCM encryption at rest.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use zeroize::Zeroize;

use cypher_common::{Error, PeerId, Result};
use cypher_crypto::RatchetState;

use super::encryption::{self, ZSTD_LEVEL};
use super::{Conversation, Direction, MessageStore, StoredMessage};

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS conversations (
        peer_id      BLOB PRIMARY KEY,
        nickname     TEXT,
        created_at   INTEGER NOT NULL,
        last_msg_at  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS messages (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        peer_id      BLOB NOT NULL REFERENCES conversations(peer_id),
        direction    INTEGER NOT NULL,
        ciphertext   BLOB NOT NULL,
        nonce        BLOB NOT NULL,
        compressed   INTEGER NOT NULL DEFAULT 0,
        timestamp    INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_messages_peer_ts
        ON messages(peer_id, timestamp);

    CREATE TABLE IF NOT EXISTS ratchet_state (
        peer_id      BLOB PRIMARY KEY,
        state_blob   BLOB NOT NULL,
        nonce        BLOB NOT NULL,
        updated_at   INTEGER NOT NULL
    );
";

/// SQLite-backed implementation of [`MessageStore`].
///
/// Every message and ratchet state blob is encrypted with the SEK (Storage
/// Encryption Key) derived from the identity seed. Messages larger than 64
/// bytes are compressed with zstd before encryption.
pub struct SqliteMessageStore {
    conn: Mutex<Connection>,
    sek: [u8; 32],
}

impl SqliteMessageStore {
    /// Open (or create) the database at the given path.
    pub fn open(db_path: impl AsRef<Path>, sek: [u8; 32]) -> Result<Self> {
        let conn =
            Connection::open(db_path).map_err(|e| Error::Crypto(format!("sqlite open: {e}")))?;
        let store = Self {
            conn: Mutex::new(conn),
            sek,
        };
        store
            .conn()?
            .execute_batch(SCHEMA)
            .map_err(|e| Error::Crypto(format!("schema init: {e}")))?;
        Ok(store)
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Crypto("sqlite mutex poisoned".into()))
    }
}

impl Drop for SqliteMessageStore {
    fn drop(&mut self) {
        self.sek.zeroize();
    }
}

impl MessageStore for SqliteMessageStore {
    fn save_message(
        &self,
        peer_id: &PeerId,
        direction: Direction,
        plaintext: &[u8],
        timestamp: u64,
    ) -> Result<u64> {
        let (ct, nonce, compressed) = encryption::compress_and_encrypt(&self.sek, plaintext)?;
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO messages (peer_id, direction, ciphertext, nonce, compressed, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                peer_id.as_bytes().as_slice(),
                direction as u8,
                ct,
                nonce.as_slice(),
                compressed as i32,
                timestamp,
            ],
        )
        .map_err(|e| Error::Crypto(format!("save_message: {e}")))?;

        let id = conn.last_insert_rowid() as u64;

        conn.execute(
            "UPDATE conversations SET last_msg_at = MAX(last_msg_at, ?1) WHERE peer_id = ?2",
            params![timestamp, peer_id.as_bytes().as_slice()],
        )
        .map_err(|e| Error::Crypto(format!("update last_msg_at: {e}")))?;

        Ok(id)
    }

    fn load_messages(
        &self,
        peer_id: &PeerId,
        limit: u32,
        before_id: Option<u64>,
    ) -> Result<Vec<StoredMessage>> {
        let bid = before_id.map(|b| b as i64).unwrap_or(i64::MAX);
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, peer_id, direction, ciphertext, nonce, compressed, timestamp
                 FROM messages WHERE peer_id = ?1 AND id < ?2
                 ORDER BY id DESC LIMIT ?3",
            )
            .map_err(|e| Error::Crypto(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(params![peer_id.as_bytes().as_slice(), bid, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, u8>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i32>(5)? != 0,
                    row.get::<_, i64>(6)? as u64,
                ))
            })
            .map_err(|e| Error::Crypto(format!("query: {e}")))?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();

        rows.into_iter()
            .map(|(id, pid, dir, ct, nonce, compressed, ts)| {
                let plaintext =
                    encryption::decrypt_and_decompress(&self.sek, &ct, &nonce, compressed)?;
                Ok(StoredMessage {
                    id,
                    peer_id: pid,
                    direction: if dir == 0 {
                        Direction::Sent
                    } else {
                        Direction::Received
                    },
                    plaintext,
                    timestamp: ts,
                })
            })
            .collect()
    }

    fn save_ratchet_state(&self, peer_id: &PeerId, state: &RatchetState) -> Result<()> {
        let serialized = state.serialize()?;
        // Skip compression for small payloads (< 1KB) where zstd overhead exceeds benefit.
        // Prefix byte: 0x00 = raw, 0x01 = zstd-compressed.
        let payload = if serialized.len() >= 1024 {
            let mut buf = vec![0x01u8];
            let compressed = zstd::encode_all(serialized.as_slice(), ZSTD_LEVEL)
                .map_err(|e| Error::Crypto(format!("zstd compress ratchet: {e}")))?;
            buf.extend_from_slice(&compressed);
            buf
        } else {
            let mut buf = Vec::with_capacity(1 + serialized.len());
            buf.push(0x00);
            buf.extend_from_slice(&serialized);
            buf
        };
        let (ct, nonce) = encryption::encrypt(&self.sek, &payload)?;
        let now = now_unix();

        self.conn()?
            .execute(
                "INSERT OR REPLACE INTO ratchet_state (peer_id, state_blob, nonce, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![peer_id.as_bytes().as_slice(), ct, nonce.as_slice(), now],
            )
            .map_err(|e| Error::Crypto(format!("save_ratchet_state: {e}")))?;
        Ok(())
    }

    fn load_ratchet_state(&self, peer_id: &PeerId) -> Result<Option<RatchetState>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT state_blob, nonce FROM ratchet_state WHERE peer_id = ?1")
            .map_err(|e| Error::Crypto(format!("prepare: {e}")))?;
        let result: Option<(Vec<u8>, Vec<u8>)> = stmt
            .query_row(params![peer_id.as_bytes().as_slice()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .ok();

        let Some((ct, nonce)) = result else {
            return Ok(None);
        };
        let payload = encryption::decrypt(&self.sek, &ct, &nonce)?;
        if payload.is_empty() {
            return Err(Error::Crypto("empty ratchet state blob".into()));
        }
        // Prefix byte: 0x00 = raw, 0x01 = zstd. Legacy data without prefix is
        // treated as zstd-compressed for backward compatibility.
        let serialized = match payload[0] {
            0x00 => payload[1..].to_vec(),
            0x01 => zstd::decode_all(&payload[1..])
                .map_err(|e| Error::Crypto(format!("zstd decompress ratchet: {e}")))?,
            _ => {
                // Legacy format: entire payload is zstd-compressed (no prefix).
                zstd::decode_all(payload.as_slice())
                    .map_err(|e| Error::Crypto(format!("zstd decompress ratchet (legacy): {e}")))?
            }
        };
        Ok(Some(RatchetState::deserialize(&serialized)?))
    }

    fn save_conversation(&self, peer_id: &PeerId, nickname: Option<&str>) -> Result<()> {
        self.conn()?
            .execute(
                "INSERT INTO conversations (peer_id, nickname, created_at, last_msg_at)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(peer_id) DO UPDATE SET nickname = COALESCE(?2, nickname)",
                params![peer_id.as_bytes().as_slice(), nickname, now_unix()],
            )
            .map_err(|e| Error::Crypto(format!("save_conversation: {e}")))?;
        Ok(())
    }

    fn list_conversations(&self) -> Result<Vec<Conversation>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT peer_id, nickname, created_at, last_msg_at
                 FROM conversations ORDER BY last_msg_at DESC",
            )
            .map_err(|e| Error::Crypto(format!("prepare: {e}")))?;

        let rows: Vec<Conversation> = stmt
            .query_map([], |row| {
                Ok(Conversation {
                    peer_id: row.get(0)?,
                    nickname: row.get(1)?,
                    created_at: row.get::<_, i64>(2)? as u64,
                    last_message_at: row.get::<_, i64>(3)? as u64,
                })
            })
            .map_err(|e| Error::Crypto(format!("query: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }

    fn delete_conversation(&self, peer_id: &PeerId) -> Result<()> {
        let conn = self.conn()?;
        let peer = peer_id.as_bytes().as_slice();
        conn.execute("DELETE FROM messages WHERE peer_id = ?1", params![peer])
            .map_err(|e| Error::Crypto(format!("delete messages: {e}")))?;
        conn.execute(
            "DELETE FROM ratchet_state WHERE peer_id = ?1",
            params![peer],
        )
        .map_err(|e| Error::Crypto(format!("delete ratchet: {e}")))?;
        conn.execute(
            "DELETE FROM conversations WHERE peer_id = ?1",
            params![peer],
        )
        .map_err(|e| Error::Crypto(format!("delete conversation: {e}")))?;
        Ok(())
    }

    fn clear_all(&self) -> Result<()> {
        self.conn()?
            .execute_batch(
                "DELETE FROM messages;
                 DELETE FROM ratchet_state;
                 DELETE FROM conversations;
                 VACUUM;",
            )
            .map_err(|e| Error::Crypto(format!("clear_all: {e}")))?;
        Ok(())
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, SqliteMessageStore) {
        let dir = TempDir::new().unwrap();
        let sek = [42u8; 32];
        let store = SqliteMessageStore::open(dir.path().join("test.db"), sek).unwrap();
        (dir, store)
    }

    fn test_peer() -> PeerId {
        PeerId([1u8; 32])
    }

    #[test]
    fn save_and_load_messages() {
        let (_dir, store) = test_store();
        let peer = test_peer();
        store.save_conversation(&peer, Some("alice")).unwrap();

        let id1 = store
            .save_message(&peer, Direction::Sent, b"hello", 1000)
            .unwrap();
        let id2 = store
            .save_message(&peer, Direction::Received, b"world", 1001)
            .unwrap();
        assert!(id2 > id1);

        let msgs = store.load_messages(&peer, 10, None).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].plaintext, b"world");
        assert_eq!(msgs[1].plaintext, b"hello");
    }

    #[test]
    fn compression_roundtrip() {
        let (_dir, store) = test_store();
        let peer = test_peer();
        store.save_conversation(&peer, None).unwrap();

        let long_msg = "a]".repeat(100);
        store
            .save_message(&peer, Direction::Sent, long_msg.as_bytes(), 2000)
            .unwrap();

        let msgs = store.load_messages(&peer, 1, None).unwrap();
        assert_eq!(msgs[0].plaintext, long_msg.as_bytes());
    }

    #[test]
    fn short_message_no_compression() {
        let (_dir, store) = test_store();
        let peer = test_peer();
        store.save_conversation(&peer, None).unwrap();

        store
            .save_message(&peer, Direction::Sent, b"hi", 3000)
            .unwrap();
        let msgs = store.load_messages(&peer, 1, None).unwrap();
        assert_eq!(msgs[0].plaintext, b"hi");
    }

    #[test]
    fn pagination() {
        let (_dir, store) = test_store();
        let peer = test_peer();
        store.save_conversation(&peer, None).unwrap();

        for i in 0..5u64 {
            store
                .save_message(
                    &peer,
                    Direction::Sent,
                    format!("msg{i}").as_bytes(),
                    4000 + i,
                )
                .unwrap();
        }

        let page1 = store.load_messages(&peer, 2, None).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].plaintext, b"msg4");

        let page2 = store.load_messages(&peer, 2, Some(page1[1].id)).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].plaintext, b"msg2");
    }

    #[test]
    fn conversations_list() {
        let (_dir, store) = test_store();
        store
            .save_conversation(&PeerId([1u8; 32]), Some("alice"))
            .unwrap();
        store
            .save_conversation(&PeerId([2u8; 32]), Some("bob"))
            .unwrap();
        assert_eq!(store.list_conversations().unwrap().len(), 2);
    }

    #[test]
    fn clear_all_empties_db() {
        let (_dir, store) = test_store();
        let peer = test_peer();
        store.save_conversation(&peer, None).unwrap();
        store
            .save_message(&peer, Direction::Sent, b"msg", 5000)
            .unwrap();

        store.clear_all().unwrap();
        assert!(store.list_conversations().unwrap().is_empty());
        assert!(store.load_messages(&peer, 10, None).unwrap().is_empty());
    }

    #[test]
    fn delete_single_conversation() {
        let (_dir, store) = test_store();
        let p1 = PeerId([1u8; 32]);
        let p2 = PeerId([2u8; 32]);
        store.save_conversation(&p1, Some("alice")).unwrap();
        store.save_conversation(&p2, Some("bob")).unwrap();
        store
            .save_message(&p1, Direction::Sent, b"hi alice", 6000)
            .unwrap();
        store
            .save_message(&p2, Direction::Sent, b"hi bob", 6001)
            .unwrap();

        store.delete_conversation(&p1).unwrap();
        assert_eq!(store.list_conversations().unwrap().len(), 1);
        assert!(store.load_messages(&p1, 10, None).unwrap().is_empty());
        assert_eq!(store.load_messages(&p2, 10, None).unwrap().len(), 1);
    }
}
