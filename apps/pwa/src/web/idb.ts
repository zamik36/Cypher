/** Minimal promise wrapper over IndexedDB for the worker's key-value tables. */
import type { Op, Row } from "../wasm/cypher_wasm.js";

const STORES = ["meta", "peers", "outbox", "transfers", "messages", "message_status", "media", "identity", "files"];
/** Bumped whenever STORES grows; the upgrade creates any missing store. */
const VERSION = 2;

/** The request's or transaction's own error, or a generic one when it has none. */
const failure = (source: IDBRequest | IDBTransaction) => source.error ?? new DOMException("aborted", "AbortError");

function done(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(failure(tx));
    tx.onabort = () => reject(failure(tx));
  });
}

function result<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(failure(req));
  });
}

export function openDb(name: string): Promise<IDBDatabase> {
  const req = indexedDB.open(name, VERSION);
  req.onupgradeneeded = () => {
    for (const store of STORES) {
      if (!req.result.objectStoreNames.contains(store)) req.result.createObjectStore(store);
    }
  };
  return result(req);
}

/** Applies a batch atomically; resolves once the transaction is durable. */
export async function applyOps(db: IDBDatabase, ops: Op[]): Promise<void> {
  if (ops.length === 0) return;
  const tables = [...new Set(ops.map((o) => o.table))];
  const tx = db.transaction(tables, "readwrite", { durability: "strict" });
  for (const op of ops) {
    const store = tx.objectStore(op.table);
    if (op.kind === "put") store.put(op.value, op.key);
    else store.delete(op.key);
  }
  await done(tx);
}

export async function scan(db: IDBDatabase, table: string, range?: IDBKeyRange, limit?: number, newestFirst = false): Promise<Row[]> {
  const tx = db.transaction(table, "readonly");
  const rows: Row[] = [];
  await new Promise<void>((resolve, reject) => {
    const req = tx.objectStore(table).openCursor(range, newestFirst ? "prev" : "next");
    req.onerror = () => reject(failure(req));
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor || (limit !== undefined && rows.length >= limit)) {
        resolve();
        return;
      }
      rows.push([new Uint8Array(cursor.key as ArrayBuffer), new Uint8Array(cursor.value as ArrayBuffer)]);
      cursor.continue();
    };
  });
  return rows;
}

export async function get<T>(db: IDBDatabase, table: string, key: IDBValidKey): Promise<T | undefined> {
  return result(db.transaction(table, "readonly").objectStore(table).get(key)) as Promise<T | undefined>;
}

export async function remove(db: IDBDatabase, table: string, key: IDBValidKey): Promise<void> {
  const tx = db.transaction(table, "readwrite");
  tx.objectStore(table).delete(key);
  await done(tx);
}

export async function put(db: IDBDatabase, table: string, key: IDBValidKey, value: unknown): Promise<void> {
  const tx = db.transaction(table, "readwrite");
  tx.objectStore(table).put(value, key);
  await done(tx);
}

export async function clear(db: IDBDatabase, tables: string[]): Promise<void> {
  const tx = db.transaction(tables, "readwrite");
  for (const t of tables) tx.objectStore(t).clear();
  await done(tx);
}
