/**
 * Chat history persistence via IndexedDB.
 *
 * Messages are stored encrypted with the Storage Encryption Key (SEK)
 * derived from the identity seed, matching the Rust SqliteMessageStore.
 */
import { encrypt, decrypt, importAesKey } from "./crypto";

const DB_NAME = "cypher-messages";
const DB_VERSION = 2;

const STORE_MESSAGES = "messages";
const STORE_CONVERSATIONS = "conversations";

export interface StoredMessage {
  id?: number; // auto-incremented
  peerId: string;
  direction: "sent" | "received";
  ciphertext: Uint8Array;
  nonce: Uint8Array;
  timestamp: number;
}

export interface Conversation {
  peerId: string;
  nickname: string | null;
  createdAt: number;
  lastMessageAt: number;
  inboxId?: string | null;
}

export interface DecryptedMessage {
  id: number;
  peerId: string;
  direction: "sent" | "received";
  text: string;
  timestamp: number;
}

let db: IDBDatabase | null = null;
let sekKey: CryptoKey | null = null;

/** Open the database and set the encryption key. */
export async function openMessageStore(sek: Uint8Array): Promise<void> {
  sekKey = await importAesKey(sek);
  db = await new Promise<IDBDatabase>((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = () => {
      const d = req.result;
      const oldVersion = req.transaction?.db.version ?? 0;
      if (!d.objectStoreNames.contains(STORE_MESSAGES)) {
        const ms = d.createObjectStore(STORE_MESSAGES, {
          keyPath: "id",
          autoIncrement: true,
        });
        ms.createIndex("peerId_timestamp", ["peerId", "timestamp"]);
      }
      if (!d.objectStoreNames.contains(STORE_CONVERSATIONS)) {
        d.createObjectStore(STORE_CONVERSATIONS, { keyPath: "peerId" });
      }
      // IndexedDB conversation records are schemaless. Version 2 reserves the
      // optional inboxId field while keeping existing records readable.
      if (oldVersion < 2 && d.objectStoreNames.contains(STORE_CONVERSATIONS)) {
        // No structural migration required.
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

/** Save a message (encrypted at rest). */
export async function saveMessage(
  peerId: string,
  direction: "sent" | "received",
  text: string,
  timestamp: number,
): Promise<void> {
  if (!db || !sekKey) return;
  const plaintext = new TextEncoder().encode(text);
  const { ciphertext, nonce } = await encrypt(sekKey, plaintext);

  const tx = db.transaction(
    [STORE_MESSAGES, STORE_CONVERSATIONS],
    "readwrite",
  );

  tx.objectStore(STORE_MESSAGES).add({
    peerId,
    direction,
    ciphertext,
    nonce,
    timestamp,
  } satisfies StoredMessage);

  // Upsert conversation.
  const convStore = tx.objectStore(STORE_CONVERSATIONS);
  const existing: Conversation | undefined = await idbGet(convStore, peerId);
  if (existing) {
    existing.lastMessageAt = Math.max(existing.lastMessageAt, timestamp);
    convStore.put(existing);
  } else {
    convStore.add({
      peerId,
      nickname: null,
      createdAt: timestamp,
      lastMessageAt: timestamp,
      inboxId: null,
    } satisfies Conversation);
  }
}

/** Load messages for a peer, decrypting them. Newest first. */
export async function loadMessages(
  peerId: string,
  limit: number = 50,
  beforeId?: number,
): Promise<DecryptedMessage[]> {
  if (!db || !sekKey) return [];

  const all: StoredMessage[] = await new Promise((resolve, reject) => {
    const tx = db!.transaction(STORE_MESSAGES, "readonly");
    const store = tx.objectStore(STORE_MESSAGES);
    const results: StoredMessage[] = [];
    const req = store.openCursor(null, "prev");
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor || results.length >= limit) {
        resolve(results);
        return;
      }
      const val = cursor.value as StoredMessage & { id: number };
      if (val.peerId === peerId) {
        if (beforeId === undefined || val.id! < beforeId) {
          results.push(val);
        }
      }
      cursor.continue();
    };
    req.onerror = () => reject(req.error);
  });

  const messages: DecryptedMessage[] = [];
  for (const msg of all) {
    try {
      const pt = await decrypt(sekKey!, msg.ciphertext, msg.nonce);
      messages.push({
        id: msg.id!,
        peerId: msg.peerId,
        direction: msg.direction,
        text: new TextDecoder().decode(pt),
        timestamp: msg.timestamp,
      });
    } catch {
      // Skip corrupted messages.
    }
  }
  return messages;
}

/** List all conversations, newest first. */
export async function listConversations(): Promise<Conversation[]> {
  if (!db) return [];
  const all: Conversation[] = await idbGetAll(
    db.transaction(STORE_CONVERSATIONS, "readonly").objectStore(STORE_CONVERSATIONS),
  );
  return all.sort((a, b) => b.lastMessageAt - a.lastMessageAt);
}

/** Save or update a conversation entry. */
export async function saveConversation(
  peerId: string,
  nickname: string | null,
  inboxId?: string | null,
): Promise<void> {
  if (!db) return;
  const tx = db.transaction(STORE_CONVERSATIONS, "readwrite");
  const store = tx.objectStore(STORE_CONVERSATIONS);
  const existing: Conversation | undefined = await idbGet(store, peerId);
  if (existing) {
    if (nickname !== null) existing.nickname = nickname;
    if (inboxId !== undefined) existing.inboxId = inboxId;
    store.put(existing);
  } else {
    const now = Date.now();
    store.add({
      peerId,
      nickname,
      createdAt: now,
      lastMessageAt: now,
      inboxId: inboxId ?? null,
    } satisfies Conversation);
  }
}

export async function getConversation(peerId: string): Promise<Conversation | undefined> {
  if (!db) return undefined;
  return idbGet<Conversation>(
    db.transaction(STORE_CONVERSATIONS, "readonly").objectStore(STORE_CONVERSATIONS),
    peerId,
  );
}

/** Delete all messages, conversations. */
export async function clearAll(): Promise<void> {
  if (!db) return;
  const tx = db.transaction(
    [STORE_MESSAGES, STORE_CONVERSATIONS],
    "readwrite",
  );
  tx.objectStore(STORE_MESSAGES).clear();
  tx.objectStore(STORE_CONVERSATIONS).clear();
}

/** Close the database. */
export function closeMessageStore(): void {
  if (db) {
    db.close();
    db = null;
  }
  sekKey = null;
}

// --- IDB helpers ---

function idbGet<T>(store: IDBObjectStore, key: IDBValidKey): Promise<T | undefined> {
  return new Promise((resolve, reject) => {
    const req = store.get(key);
    req.onsuccess = () => resolve(req.result as T | undefined);
    req.onerror = () => reject(req.error);
  });
}

function idbGetAll<T>(store: IDBObjectStore): Promise<T[]> {
  return new Promise((resolve, reject) => {
    const req = store.getAll();
    req.onsuccess = () => resolve(req.result as T[]);
    req.onerror = () => reject(req.error);
  });
}
