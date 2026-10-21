//! Ordered key-value store on SQLite, owned by one blocking thread so the
//! async runtime never waits on disk. Every batch commits atomically.

use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::thread;

use cypher_core::{CoreError, StoreOp, Table, status_key};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::oneshot;

use crate::ClientError;

/// Driver-owned table: file id → local path of a transfer.
pub(crate) const FILES_TABLE: StoreTable = StoreTable::Files;
/// Driver-owned table: file id → where a finished file was saved.
pub(crate) const SAVED_TABLE: StoreTable = StoreTable::Saved;

use cypher_core::Rows as Pairs;

/// A table of the store: one the core keeps its state in, or one of the
/// driver's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreTable {
    Core(Table),
    Files,
    Saved,
}

impl From<Table> for StoreTable {
    fn from(table: Table) -> Self {
        Self::Core(table)
    }
}

/// Every statement the store runs on one table. Table names cannot be SQL
/// parameters, so each table has its own constant text; values always are
/// parameters.
struct Sql {
    create: &'static str,
    scan: &'static str,
    range_before: &'static str,
    range_through: &'static str,
    after: &'static str,
    get: &'static str,
    put: &'static str,
    put_if_absent: &'static str,
    delete: &'static str,
    clear: &'static str,
}

macro_rules! sql {
    ($name:literal) => {
        Sql {
            create: concat!(
                "CREATE TABLE IF NOT EXISTS ",
                $name,
                " (k BLOB PRIMARY KEY, v BLOB NOT NULL) WITHOUT ROWID"
            ),
            scan: concat!("SELECT k, v FROM ", $name),
            range_before: concat!(
                "SELECT k, v FROM ",
                $name,
                " WHERE k >= ?1 AND k < ?2 ORDER BY k DESC LIMIT ?3"
            ),
            range_through: concat!(
                "SELECT k, v FROM ",
                $name,
                " WHERE k >= ?1 AND k <= ?2 ORDER BY k DESC LIMIT ?3"
            ),
            after: concat!(
                "SELECT k, v FROM ",
                $name,
                " WHERE k > ?1 ORDER BY k LIMIT ?2"
            ),
            get: concat!("SELECT v FROM ", $name, " WHERE k = ?1"),
            put: concat!("INSERT OR REPLACE INTO ", $name, " (k, v) VALUES (?1, ?2)"),
            put_if_absent: concat!("INSERT OR IGNORE INTO ", $name, " (k, v) VALUES (?1, ?2)"),
            delete: concat!("DELETE FROM ", $name, " WHERE k = ?1"),
            clear: concat!("DELETE FROM ", $name),
        }
    };
}

const META: Sql = sql!("meta");
const PEERS: Sql = sql!("peers");
const OUTBOX: Sql = sql!("outbox");
const TRANSFERS: Sql = sql!("transfers");
const MESSAGES: Sql = sql!("messages");
const MESSAGE_STATUS: Sql = sql!("message_status");
const MEDIA: Sql = sql!("media");
const SESSIONS: Sql = sql!("sessions");
const FILES: Sql = sql!("driver_files");
const SAVED: Sql = sql!("driver_saved");

impl StoreTable {
    const ALL: [Self; 10] = [
        Self::Core(Table::Meta),
        Self::Core(Table::Peers),
        Self::Core(Table::Outbox),
        Self::Core(Table::Transfers),
        Self::Core(Table::Messages),
        Self::Core(Table::MessageStatus),
        Self::Core(Table::Media),
        Self::Core(Table::Sessions),
        Self::Files,
        Self::Saved,
    ];

    fn sql(self) -> &'static Sql {
        match self {
            Self::Core(Table::Meta) => &META,
            Self::Core(Table::Peers) => &PEERS,
            Self::Core(Table::Outbox) => &OUTBOX,
            Self::Core(Table::Transfers) => &TRANSFERS,
            Self::Core(Table::Messages) => &MESSAGES,
            Self::Core(Table::MessageStatus) => &MESSAGE_STATUS,
            Self::Core(Table::Media) => &MEDIA,
            Self::Core(Table::Sessions) => &SESSIONS,
            Self::Files => &FILES,
            Self::Saved => &SAVED,
        }
    }
}

enum Job {
    Apply(Vec<Op>, oneshot::Sender<Result<(), ClientError>>),
    Scan(StoreTable, oneshot::Sender<Result<Pairs, ClientError>>),
    Range {
        table: StoreTable,
        from: Vec<u8>,
        to: End,
        limit: usize,
        reply: oneshot::Sender<Result<Pairs, ClientError>>,
    },
    After {
        table: StoreTable,
        after: Vec<u8>,
        limit: usize,
        reply: oneshot::Sender<Result<Pairs, ClientError>>,
    },
    Messages {
        spans: Vec<(Vec<u8>, End)>,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<Vec<MessageRow>>, ClientError>>,
    },
    Get(
        StoreTable,
        Vec<u8>,
        oneshot::Sender<Result<Option<Vec<u8>>, ClientError>>,
    ),
}

/// A message row, its value and the row of its latest status, if any.
pub(crate) type MessageRow = (Vec<u8>, Vec<u8>, Option<Vec<u8>>);

/// Where a range of keys ends.
pub(crate) enum End {
    /// Just before this key.
    Before(Vec<u8>),
    /// At this key, which is still part of the range.
    Through(Vec<u8>),
}

pub(crate) enum Op {
    Put(StoreTable, Vec<u8>, Vec<u8>),
    /// Kept only where the key is free: what is stored stays.
    PutIfAbsent(StoreTable, Vec<u8>, Vec<u8>),
    Delete(StoreTable, Vec<u8>),
    Clear(StoreTable),
}

impl From<StoreOp> for Op {
    fn from(op: StoreOp) -> Self {
        match op {
            StoreOp::Put { table, key, value } => Self::Put(table.into(), key, value),
            StoreOp::Delete { table, key } => Self::Delete(table.into(), key),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Store {
    tx: std_mpsc::Sender<Job>,
}

/// Layout of the database: its tables and key formats. Bump it when they
/// change and migrate in [`Store::open`]; a database from a newer release
/// is refused rather than written in a layout it does not expect.
const SCHEMA_VERSION: u32 = 2;

impl Store {
    pub(crate) fn open(path: &Path) -> Result<Self, ClientError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let found: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if found > SCHEMA_VERSION {
            return Err(CoreError::NewerStorage.into());
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        for table in StoreTable::ALL {
            conn.execute(table.sql().create, [])?;
        }
        // Unbounded by design: every caller waits for its reply, so it holds
        // at most one job per waiting task.
        let (tx, rx) = std_mpsc::channel();
        thread::Builder::new()
            .name("cypher-store".into())
            .spawn(move || serve(conn, &rx))?;
        Ok(Self { tx })
    }

    /// A store whose worker is gone, as after the database thread died:
    /// every call fails with [`ClientError::Closed`].
    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        let (tx, _) = std_mpsc::channel();
        Self { tx }
    }

    /// Resolves once the batch is durable.
    pub(crate) async fn apply(&self, ops: Vec<Op>) -> Result<(), ClientError> {
        if ops.is_empty() {
            return Ok(());
        }
        self.call(|reply| Job::Apply(ops, reply)).await
    }

    pub(crate) async fn scan(&self, table: StoreTable) -> Result<Pairs, ClientError> {
        self.call(|reply| Job::Scan(table, reply)).await
    }

    /// Keys from `from` to `to`, newest (largest) first.
    pub(crate) async fn range_desc(
        &self,
        table: StoreTable,
        from: Vec<u8>,
        to: End,
        limit: usize,
    ) -> Result<Pairs, ClientError> {
        self.call(|reply| Job::Range {
            table,
            from,
            to,
            limit,
            reply,
        })
        .await
    }

    pub(crate) async fn get(
        &self,
        table: StoreTable,
        key: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, ClientError> {
        self.call(|reply| Job::Get(table, key, reply)).await
    }

    /// Up to `limit` rows with keys after `after`, in key order, for a
    /// thread that may block (never one running async tasks).
    pub(crate) fn blocking_after(
        &self,
        table: StoreTable,
        after: Vec<u8>,
        limit: usize,
    ) -> Result<Pairs, ClientError> {
        self.call_blocking(|reply| Job::After {
            table,
            after,
            limit,
            reply,
        })
    }

    /// [`Store::range_desc`] for a thread that may block.
    pub(crate) fn blocking_range(
        &self,
        table: StoreTable,
        from: Vec<u8>,
        to: End,
        limit: usize,
    ) -> Result<Pairs, ClientError> {
        self.call_blocking(|reply| Job::Range {
            table,
            from,
            to,
            limit,
            reply,
        })
    }

    /// [`Store::get`] for a thread that may block.
    pub(crate) fn blocking_get(
        &self,
        table: StoreTable,
        key: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, ClientError> {
        self.call_blocking(|reply| Job::Get(table, key, reply))
    }

    /// [`Store::scan`] for a thread that may block.
    pub(crate) fn blocking_scan(&self, table: StoreTable) -> Result<Pairs, ClientError> {
        self.call_blocking(|reply| Job::Scan(table, reply))
    }

    /// Up to `limit` messages of each span `(from, to)`, newest first, each
    /// with the row of its status: one job for them all, for a thread that
    /// may block.
    pub(crate) fn blocking_messages(
        &self,
        spans: Vec<(Vec<u8>, End)>,
        limit: usize,
    ) -> Result<Vec<Vec<MessageRow>>, ClientError> {
        self.call_blocking(|reply| Job::Messages {
            spans,
            limit,
            reply,
        })
    }

    /// [`Store::apply`] for a thread that may block.
    pub(crate) fn blocking_apply(&self, ops: Vec<Op>) -> Result<(), ClientError> {
        if ops.is_empty() {
            return Ok(());
        }
        self.call_blocking(|reply| Job::Apply(ops, reply))
    }

    async fn call<T>(
        &self,
        job: impl FnOnce(oneshot::Sender<Result<T, ClientError>>) -> Job,
    ) -> Result<T, ClientError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(job(reply)).map_err(|_| ClientError::Closed)?;
        rx.await.map_err(|_| ClientError::Closed)?
    }

    fn call_blocking<T>(
        &self,
        job: impl FnOnce(oneshot::Sender<Result<T, ClientError>>) -> Job,
    ) -> Result<T, ClientError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(job(reply)).map_err(|_| ClientError::Closed)?;
        rx.blocking_recv().map_err(|_| ClientError::Closed)?
    }
}

fn serve(mut conn: Connection, rx: &std_mpsc::Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Apply(ops, reply) => {
                let _ = reply.send(apply(&mut conn, &ops));
            }
            Job::Scan(table, reply) => {
                let _ = reply.send(query(&conn, table.sql().scan, params![]));
            }
            Job::Range {
                table,
                from,
                to,
                limit,
                reply,
            } => {
                let (sql, to) = match to {
                    End::Before(to) => (table.sql().range_before, to),
                    End::Through(to) => (table.sql().range_through, to),
                };
                let limit = i64::try_from(limit).unwrap_or(i64::MAX);
                let _ = reply.send(query(&conn, sql, params![from, to, limit]));
            }
            Job::After {
                table,
                after,
                limit,
                reply,
            } => {
                let limit = i64::try_from(limit).unwrap_or(i64::MAX);
                let _ = reply.send(query(&conn, table.sql().after, params![after, limit]));
            }
            Job::Messages {
                spans,
                limit,
                reply,
            } => {
                let _ = reply.send(messages(&conn, spans, limit));
            }
            Job::Get(table, key, reply) => {
                let res = conn
                    .prepare_cached(table.sql().get)
                    .and_then(|mut stmt| stmt.query_row(params![key], |r| r.get(0)))
                    .optional()
                    .map_err(ClientError::from);
                let _ = reply.send(res);
            }
        }
    }
}

fn apply(conn: &mut Connection, ops: &[Op]) -> Result<(), ClientError> {
    let tx = conn.transaction()?;
    for op in ops {
        match op {
            Op::Put(table, k, v) => {
                tx.prepare_cached(table.sql().put)?.execute(params![k, v])?;
            }
            Op::PutIfAbsent(table, k, v) => {
                tx.prepare_cached(table.sql().put_if_absent)?
                    .execute(params![k, v])?;
            }
            Op::Delete(table, k) => {
                tx.prepare_cached(table.sql().delete)?.execute(params![k])?;
            }
            Op::Clear(table) => {
                tx.execute(table.sql().clear, [])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// Each span's messages, newest first, with their status rows.
fn messages(
    conn: &Connection,
    spans: Vec<(Vec<u8>, End)>,
    limit: usize,
) -> Result<Vec<Vec<MessageRow>>, ClientError> {
    let messages = StoreTable::from(Table::Messages).sql();
    let mut status = conn.prepare_cached(StoreTable::from(Table::MessageStatus).sql().get)?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    spans
        .into_iter()
        .map(|(from, to)| {
            let (sql, to) = match to {
                End::Before(to) => (messages.range_before, to),
                End::Through(to) => (messages.range_through, to),
            };
            query(conn, sql, params![from, to, limit])?
                .into_iter()
                .map(|(key, value)| {
                    let found = status
                        .query_row(params![status_key(&key)], |r| r.get(0))
                        .optional()?;
                    Ok((key, value, found))
                })
                .collect()
        })
        .collect()
}

fn query(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Pairs, ClientError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(args, |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn batches_scans_and_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db")).unwrap();
        let t = StoreTable::from(Table::Messages);
        store
            .apply(vec![
                Op::Put(t, vec![1, 1], b"a".to_vec()),
                Op::Put(t, vec![1, 2], b"b".to_vec()),
                Op::Put(t, vec![2, 1], b"c".to_vec()),
            ])
            .await
            .unwrap();
        store.apply(vec![Op::Delete(t, vec![1, 1])]).await.unwrap();
        assert_eq!(store.scan(t).await.unwrap().len(), 2);
        let range = store
            .range_desc(t, vec![1], End::Before(vec![2, 1]), 10)
            .await
            .unwrap();
        assert_eq!(range, vec![(vec![1, 2], b"b".to_vec())]);
        let range = store
            .range_desc(t, vec![1], End::Through(vec![2, 1]), 10)
            .await
            .unwrap();
        assert_eq!(range.len(), 2);
        assert_eq!(
            range.first().map(|(k, _)| k.as_slice()),
            Some([2, 1].as_slice())
        );
        assert_eq!(store.get(t, vec![2, 1]).await.unwrap(), Some(b"c".to_vec()));
        assert_eq!(store.get(t, vec![9]).await.unwrap(), None);
    }

    #[tokio::test]
    async fn pages_in_key_order_and_keeps_what_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db")).unwrap();
        let t = StoreTable::from(Table::Messages);
        let worker = store.clone();
        let pages = tokio::task::spawn_blocking(move || {
            worker
                .blocking_apply(vec![
                    Op::Put(t, vec![2], b"live".to_vec()),
                    Op::PutIfAbsent(t, vec![2], b"older".to_vec()),
                    Op::PutIfAbsent(t, vec![1], b"a".to_vec()),
                    Op::PutIfAbsent(t, vec![3], b"c".to_vec()),
                ])
                .unwrap();
            let first = worker.blocking_after(t, Vec::new(), 2).unwrap();
            let rest = worker.blocking_after(t, first[1].0.clone(), 2).unwrap();
            (first, rest)
        })
        .await
        .unwrap();
        assert_eq!(
            pages.0,
            vec![(vec![1], b"a".to_vec()), (vec![2], b"live".to_vec())]
        );
        assert_eq!(pages.1, vec![(vec![3], b"c".to_vec())]);
    }

    /// Each table's statements name it as the rest of the code does.
    #[test]
    fn each_table_has_its_own_statements() {
        for table in StoreTable::ALL {
            let sql = table.sql();
            let name = match table {
                StoreTable::Core(t) => t.name(),
                StoreTable::Files => "driver_files",
                StoreTable::Saved => "driver_saved",
            };
            for text in [
                sql.create,
                sql.scan,
                sql.range_before,
                sql.range_through,
                sql.after,
                sql.get,
                sql.put,
                sql.put_if_absent,
                sql.delete,
                sql.clear,
            ] {
                assert!(
                    text.contains(&format!(" {name} ")) || text.ends_with(&format!(" {name}")),
                    "{text}"
                );
            }
        }
        let core: Vec<Table> = StoreTable::ALL
            .into_iter()
            .filter_map(|t| match t {
                StoreTable::Core(t) => Some(t),
                StoreTable::Files | StoreTable::Saved => None,
            })
            .collect();
        assert_eq!(core, Table::ALL, "every core table is created");
    }

    #[tokio::test]
    async fn messages_come_with_their_status_rows_in_one_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db")).unwrap();
        let (m, s) = (Table::Messages.into(), Table::MessageStatus.into());
        let key = |peer: u8, id: u8| [vec![peer; 32], vec![id; 8], vec![id; 16]].concat();
        let mut ops = Vec::new();
        for peer in [1, 2] {
            for id in 1..=3 {
                ops.push(Op::Put(m, key(peer, id), vec![peer, id]));
            }
        }
        ops.push(Op::Put(s, vec![2; 16], b"read".to_vec()));
        store.apply(ops).await.unwrap();

        let worker = store.clone();
        let spans = vec![
            (key(1, 0), End::Through(key(1, 9))),
            (key(2, 0), End::Before(key(2, 3))),
        ];
        let got = tokio::task::spawn_blocking(move || worker.blocking_messages(spans, 2))
            .await
            .unwrap()
            .unwrap();
        let shown: Vec<Vec<_>> = got
            .into_iter()
            .map(|rows| rows.into_iter().map(|(_, v, s)| (v, s)).collect())
            .collect();
        let read = Some(b"read".to_vec());
        assert_eq!(
            shown,
            [
                vec![(vec![1, 3], None), (vec![1, 2], read.clone())],
                vec![(vec![2, 2], read), (vec![2, 1], None)],
            ]
        );
    }

    #[test]
    fn a_database_from_a_newer_release_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        drop(Store::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            Store::open(&path),
            Err(ClientError::Core(CoreError::NewerStorage))
        ));
    }
}
