/** Minimal promise wrapper over IndexedDB for the worker's key-value tables. */

const STORES = ["meta", "peers", "outbox", "transfers", "messages", "message_status", "identity", "files"];

export type Bytes = Uint8Array<ArrayBuffer>;
export type Row = [Bytes, Bytes];

function done(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
  });
}

function result<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

export function openDb(name: string): Promise<IDBDatabase> {
  const req = indexedDB.open(name, 1);
  req.onupgradeneeded = () => {
    for (const store of STORES) {
      if (!req.result.objectStoreNames.contains(store)) req.result.createObjectStore(store);
    }
  };
  return result(req);
}

export type Op =
  | { kind: "put"; table: string; key: Bytes; value: Bytes }
  | { kind: "delete"; table: string; key: Bytes };

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
    req.onerror = () => reject(req.error);
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor || (limit !== undefined && rows.length >= limit)) return resolve();
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
