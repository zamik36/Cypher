//! Ordered key-value store on SQLite, owned by one blocking thread so the
//! async runtime never waits on disk. Every batch commits atomically.

use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::thread;

use cypher_core::{StoreOp, Table};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::oneshot;

use crate::ClientError;

/// Driver-owned table: file id → local path of a transfer.
pub const FILES_TABLE: &str = "driver_files";

type Pairs = Vec<(Vec<u8>, Vec<u8>)>;

enum Job {
    Apply(Vec<Op>, oneshot::Sender<Result<(), ClientError>>),
    Scan(&'static str, oneshot::Sender<Result<Pairs, ClientError>>),
    Range {
        table: &'static str,
        from: Vec<u8>,
        to: Vec<u8>,
        limit: usize,
        reply: oneshot::Sender<Result<Pairs, ClientError>>,
    },
    Get(
        &'static str,
        Vec<u8>,
        oneshot::Sender<Result<Option<Vec<u8>>, ClientError>>,
    ),
}

pub enum Op {
    Put(&'static str, Vec<u8>, Vec<u8>),
    Delete(&'static str, Vec<u8>),
    Clear(&'static str),
}

impl From<StoreOp> for Op {
    fn from(op: StoreOp) -> Self {
        match op {
            StoreOp::Put { table, key, value } => Self::Put(table.name(), key, value),
            StoreOp::Delete { table, key } => Self::Delete(table.name(), key),
        }
    }
}

#[derive(Clone)]
pub struct Store {
    tx: std_mpsc::Sender<Job>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, ClientError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        for name in Table::ALL.iter().map(|t| t.name()).chain([FILES_TABLE]) {
            conn.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {name} (k BLOB PRIMARY KEY, v BLOB NOT NULL) WITHOUT ROWID;"
            ))?;
        }
        let (tx, rx) = std_mpsc::channel();
        thread::Builder::new()
            .name("cypher-store".into())
            .spawn(move || serve(conn, &rx))?;
        Ok(Self { tx })
    }

    /// Resolves once the batch is durable.
    pub async fn apply(&self, ops: Vec<Op>) -> Result<(), ClientError> {
        if ops.is_empty() {
            return Ok(());
        }
        self.call(|reply| Job::Apply(ops, reply)).await
    }

    pub async fn scan(&self, table: &'static str) -> Result<Pairs, ClientError> {
        self.call(|reply| Job::Scan(table, reply)).await
    }

    /// Keys in `[from, to)`, newest (largest) first.
    pub async fn range_desc(
        &self,
        table: &'static str,
        from: Vec<u8>,
        to: Vec<u8>,
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

    pub async fn get(
        &self,
        table: &'static str,
        key: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, ClientError> {
        self.call(|reply| Job::Get(table, key, reply)).await
    }

    async fn call<T>(
        &self,
        job: impl FnOnce(oneshot::Sender<Result<T, ClientError>>) -> Job,
    ) -> Result<T, ClientError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(job(reply)).map_err(|_| ClientError::Closed)?;
        rx.await.map_err(|_| ClientError::Closed)?
    }
}

fn serve(mut conn: Connection, rx: &std_mpsc::Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Apply(ops, reply) => {
                let _ = reply.send(apply(&mut conn, &ops));
            }
            Job::Scan(table, reply) => {
                let _ = reply.send(query(
                    &conn,
                    &format!("SELECT k, v FROM {table}"),
                    params![],
                ));
            }
            Job::Range {
                table,
                from,
                to,
                limit,
                reply,
            } => {
                let sql = format!(
                    "SELECT k, v FROM {table} WHERE k >= ?1 AND k < ?2 ORDER BY k DESC LIMIT ?3"
                );
                let limit = i64::try_from(limit).unwrap_or(i64::MAX);
                let _ = reply.send(query(&conn, &sql, params![from, to, limit]));
            }
            Job::Get(table, key, reply) => {
                let sql = format!("SELECT v FROM {table} WHERE k = ?1");
                let res = conn
                    .query_row(&sql, params![key], |r| r.get(0))
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
                tx.prepare_cached(&format!(
                    "INSERT OR REPLACE INTO {table} (k, v) VALUES (?1, ?2)"
                ))?
                .execute(params![k, v])?;
            }
            Op::Delete(table, k) => {
                tx.prepare_cached(&format!("DELETE FROM {table} WHERE k = ?1"))?
                    .execute(params![k])?;
            }
            Op::Clear(table) => {
                tx.execute(&format!("DELETE FROM {table}"), [])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
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
        let t = Table::Messages.name();
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
        let range = store.range_desc(t, vec![1], vec![2], 10).await.unwrap();
        assert_eq!(range, vec![(vec![1, 2], b"b".to_vec())]);
        assert_eq!(store.get(t, vec![2, 1]).await.unwrap(), Some(b"c".to_vec()));
        assert_eq!(store.get(t, vec![9]).await.unwrap(), None);
    }
}
