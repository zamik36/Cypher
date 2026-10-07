/// <reference lib="webworker" />
import init, {
  Client,
  historyRange,
  Identity,
  qrSvg,
  waveformFromRms,
  type Effect,
  type ReadChunk,
  type Reply,
  type Op,
  type SealedIdentity,
} from "../wasm/cypher_wasm.js";
import { applyOps, clear, get, openDb, put, remove, scan, STORES } from "./idb";
import { applyEffects, type EffectSinks } from "./effects";
import type { Method, Methods, Request, WorkerMessage } from "./protocol";

declare const self: DedicatedWorkerGlobalScope;

const MIN_BACKOFF_MS = 1000;
const MAX_BACKOFF_MS = 30_000;
const TICK_MS = 500;
const REPLY_TIMEOUT_MS = 20_000;
/** Media up to this size travels inside the message (envelope limit). */
const MAX_INLINE_BYTES = 32 * 1024;
const IDENTITY_KEY = "current";

const ready = init().then(() => openDb("cypher"));
let db: IDBDatabase;
let identity: Identity | null = null;
let client: Client | null = null;
let queue: Promise<unknown> = Promise.resolve();

const sources = new Map<string, File>();
const offerNames = new Map<string, string>();
const openSinks = new Map<string, { handle: FileSystemSyncAccessHandle; name: string; sealed: boolean }>();
const waiters = new Set<(r: Reply) => boolean>();

const post = (msg: WorkerMessage) => self.postMessage(msg);

/** Runs core interactions one at a time so effects apply in order. */
function serial<T>(task: () => Promise<T> | T): Promise<T> {
  const next = queue.then(task, task);
  queue = next.catch(() => undefined);
  return next;
}

function feed(call: (c: Client, now: number) => Effect[]): Promise<void> {
  return serial(async () => {
    if (client) await apply(call(client, Date.now()));
  });
}

function requireIdentity(): Identity {
  if (!identity) throw new Error("identity is locked");
  return identity;
}

function requireClient(): Client {
  if (!client) throw new Error("not connected");
  return client;
}

const sinks: EffectSinks = {
  persist: (ops) => applyOps(db, ops),
  transmit: (data) => gateway.send(data),
  anonymous: (data) => relay.send(data),
  event: onEvent,
  reply: (r) => {
    for (const w of waiters) if (w(r)) waiters.delete(w);
  },
  readChunk: (request) => void readChunk(request),
  openSink,
  writeChunk: (fileId, offset, data) => openSinks.get(fileId)?.handle.write(data, { at: offset }),
  closeSink,
  disconnect: (reconnect) => gateway.restart(reconnect),
};

const apply = (effects: Effect[]) => applyEffects(effects, sinks);

function onEvent(channel: string, payload: unknown) {
  switch (channel) {
    case "file_offered": {
      const offer = payload as { file_id: string; name: string };
      offerNames.set(offer.file_id, offer.name);
      break;
    }
    case "file_complete":
    case "file_failed": {
      const fileId = typeof payload === "string" ? payload : (payload as { file_id: string }).file_id;
      if (sources.delete(fileId)) void remove(db, "files", fileId);
      break;
    }
  }
  post({ event: channel, payload });
}

/** Resolves with the first reply `pick` accepts; register before issuing the command. */
function awaitReply(pick: (r: Reply) => string | Error | undefined): Promise<string> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      waiters.delete(waiter);
      reject(new Error("timed out"));
    }, REPLY_TIMEOUT_MS);
    const waiter = (r: Reply) => {
      const out = pick(r);
      if (out === undefined) return false;
      clearTimeout(timer);
      if (out instanceof Error) reject(out);
      else resolve(out);
      return true;
    };
    waiters.add(waiter);
  });
}

/** A reconnecting WebSocket carrying one wire frame per binary message. */
class Link {
  private ws: WebSocket | null = null;
  private url = "";
  /** The address to resume after the core stopped reconnecting. */
  private lastUrl = "";
  private backoff = MIN_BACKOFF_MS;
  private timer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private readonly onOpen: () => void,
    private readonly onFrame: (data: Uint8Array) => void,
    private readonly onClose: () => void,
  ) {}

  start(url: string) {
    this.stop();
    this.url = url;
    this.lastUrl = url;
    this.backoff = MIN_BACKOFF_MS;
    this.connect();
  }

  /** Detaches silently: the caller resets the core itself. */
  stop() {
    clearTimeout(this.timer);
    this.url = "";
    const ws = this.ws;
    this.ws = null;
    ws?.close();
  }

  /** Closes the socket at the core's request, reconnecting if asked. */
  restart(reconnect: boolean) {
    if (!reconnect) this.url = "";
    this.ws?.close();
  }

  /** Connects again after `restart(false)`; does nothing while connected. */
  resume() {
    if (this.url || !this.lastUrl) return;
    this.url = this.lastUrl;
    this.backoff = MIN_BACKOFF_MS;
    this.connect();
  }

  send(data: Uint8Array) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(data);
  }

  private connect() {
    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.backoff = MIN_BACKOFF_MS;
      this.onOpen();
    };
    ws.onmessage = (m) => {
      if (this.ws === ws) this.onFrame(new Uint8Array(m.data as ArrayBuffer));
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.onClose();
      if (!this.url) return;
      // Upper half of the backoff, at random, so a crowd does not reconnect in step.
      this.timer = setTimeout(() => this.connect(), this.backoff * (0.5 + Math.random() / 2));
      this.backoff = Math.min(this.backoff * 2, MAX_BACKOFF_MS);
    };
    this.ws = ws;
  }
}

const gateway = new Link(
  () => void feed((c, now) => c.connected(now)),
  (data) => void feed((c, now) => c.frame(data, now)),
  () => void feed((c, now) => c.disconnected(now)),
);

const relay = new Link(
  () => void feed((c, now) => c.anonymousChannel(true, now)),
  (data) => void feed((c, now) => c.anonymousFrame(data, now)),
  () => void feed((c, now) => c.anonymousChannel(false, now)),
);

async function readChunk(e: ReadChunk) {
  const file = sources.get(e.file_id) ?? (await get<File>(db, "files", e.file_id));
  if (!file) return feed((c, now) => c.chunkUnavailable(e.file_id, now));
  sources.set(e.file_id, file);
  const data = new Uint8Array(await file.slice(e.offset, e.offset + e.len).arrayBuffer());
  const buf = new Uint8Array(e.headroom + data.length);
  buf.set(data, e.headroom);
  return feed((c, now) => c.chunkRead(e.file_id, e.index, buf, now));
}

async function sinkDir(sealed: boolean) {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(sealed ? "media" : "downloads", { create: true });
}

async function openSink(fileId: string, len: number, sealed: boolean) {
  if (openSinks.has(fileId)) return;
  const file = await (await sinkDir(sealed)).getFileHandle(fileId, { create: true });
  const handle = await file.createSyncAccessHandle();
  if (handle.getSize() !== len) handle.truncate(len);
  openSinks.set(fileId, { handle, name: offerNames.get(fileId) ?? fileId, sealed });
}

async function closeSink(fileId: string, complete: boolean) {
  const sink = openSinks.get(fileId);
  if (!sink) return;
  openSinks.delete(fileId);
  offerNames.delete(fileId);
  sink.handle.flush();
  sink.handle.close();
  const dir = await sinkDir(sink.sealed);
  if (!complete) {
    await dir.removeEntry(fileId).catch(() => undefined);
  } else if (!sink.sealed) {
    const blob = await (await dir.getFileHandle(fileId)).getFile();
    post({ download: { fileId, name: sink.name, blob } });
  }
}

async function sealedBlob(): Promise<Uint8Array> {
  const blob = await get<Uint8Array>(db, "identity", IDENTITY_KEY);
  if (!blob) throw new Error("no identity");
  return blob;
}

async function adopt(sealed: SealedIdentity): Promise<string> {
  if ((await get<Uint8Array>(db, "identity", IDENTITY_KEY)) !== undefined)
    throw new Error("an identity already exists");
  await put(db, "identity", IDENTITY_KEY, sealed.blob);
  identity?.free();
  identity = sealed.intoIdentity();
  return identity.peerId();
}

async function startClient(): Promise<Client> {
  if (!identity) throw new Error("identity is locked");
  const [meta, peers, sessions, outbox, transfers] = await Promise.all([
    scan(db, "meta"),
    scan(db, "peers"),
    scan(db, "sessions"),
    scan(db, "outbox"),
    scan(db, "transfers"),
  ]);
  client?.free();
  client = new Client(identity, { meta, peers, sessions, outbox, transfers }, Date.now());
  await apply(client.startupEffects());
  return client;
}

async function run(cmd: Record<string, unknown>) {
  const { effects, ...ids } = requireClient().command(cmd, Date.now());
  await apply(effects);
  return ids;
}

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const unhex = (h: string) => Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));

async function sendMedia(
  peer: string,
  blob: Blob,
  mime: string,
  kind: "voice" | "video_note",
  durationMs: number,
  extra: { frames?: Float32Array; poster?: Uint8Array },
) {
  const waveform = kind === "voice" ? Array.from(waveformFromRms(extra.frames ?? new Float32Array())) : undefined;
  const inline = blob.size <= MAX_INLINE_BYTES ? new Uint8Array(await blob.arrayBuffer()) : undefined;
  const name = `${kind === "voice" ? "voice" : "note"}.${mime.includes("mp4") ? "mp4" : mime.includes("ogg") ? "ogg" : "webm"}`;
  return serial(async () => {
    const { msgId, fileId } = await run({
      type: "send_file",
      peer,
      name,
      mime,
      size: blob.size,
      kind,
      duration_ms: durationMs,
      waveform,
      poster: extra.poster,
      inline,
    });
    if (!msgId || !fileId) throw new Error("media rejected");
    if (!inline) {
      const file = new File([blob], name, { type: mime });
      sources.set(fileId, file);
      await put(db, "files", fileId, file);
    }
    return { msg_id: msgId, file_id: fileId, duration_ms: durationMs, ...(waveform && { waveform }) };
  });
}

async function mediaBlob(fileId: string): Promise<Blob> {
  const record = await get<Uint8Array>(db, "media", unhex(fileId));
  if (!record) throw new Error("media not found");
  const file = await (await (await sinkDir(true)).getFileHandle(fileId)).getFile();
  const sealed = new Uint8Array(await file.arrayBuffer());
  const { mime, bytes } = requireClient().openMedia(fileId, record, sealed);
  return new Blob([bytes], { type: mime });
}

async function history(peer: string, limit: number, before?: number) {
  const c = requireClient();
  const [from, to] = historyRange(peer, before);
  const rows = await scan(db, "messages", IDBKeyRange.bound(from, to, false, true), limit, true);
  const statuses = await Promise.all(
    rows.map(([key]) => get<Uint8Array>(db, "message_status", key.subarray(key.length - 16))),
  );
  return rows.map(([key, value], i) => c.openMessage(key, value, statuses[i]));
}

/** Stops talking to the network and drops every key from memory. */
function forget() {
  gateway.stop();
  relay.stop();
  for (const sink of openSinks.values()) sink.handle.close();
  openSinks.clear();
  sources.clear();
  offerNames.clear();
  client?.free();
  client = null;
  identity?.free();
  identity = null;
}

/** How far around the UI's time a deleted message is looked for. */
const DELETE_WINDOW_MS = 60_000;

/** Deletes stored messages with their statuses, media notes and kept files. */
async function deleteRows(rows: Awaited<ReturnType<typeof scan>>) {
  const c = requireClient();
  const media: string[] = [];
  const received: string[] = [];
  const ops: Op[] = [];
  for (const [key, value] of rows) {
    const msg = c.openMessage(key, value, undefined) as StoredView;
    if (msg.file && msg.file.kind !== "file") media.push(msg.file.file_id);
    else if (msg.file) received.push(msg.file.file_id);
    ops.push({ kind: "delete", table: "messages", key });
    ops.push({ kind: "delete", table: "message_status", key: key.slice(key.length - 16) });
  }
  for (const id of media) ops.push({ kind: "delete", table: "media", key: unhex(id) });
  await applyOps(db, ops);
  const [sealed, plain] = await Promise.all([sinkDir(true), sinkDir(false)]);
  await Promise.all([
    ...media.map((id) => sealed.removeEntry(id).catch(() => undefined)),
    ...received.map((id) => plain.removeEntry(id).catch(() => undefined)),
  ]);
}

/** How far back unread messages are counted; the list shows "99+" anyway. */
const UNREAD_WINDOW = 100;

/** The fields of a decrypted message the worker itself reads. */
interface StoredView {
  timestamp: number;
  outgoing: boolean;
  status: string;
  file: { file_id: string; kind: string } | null;
}

type Handlers = { [M in Method]: (...args: Parameters<Methods[M]>) => Promise<ReturnType<Methods[M]>> };

const handlers: Handlers = {
  hasIdentity: async () => (await get<Uint8Array>(db, "identity", IDENTITY_KEY)) !== undefined,
  createIdentity: (nickname, passphrase) => adopt(Identity.create(nickname, passphrase)),
  importMnemonic: (mnemonic, nickname, passphrase) => adopt(Identity.import(mnemonic, nickname, passphrase)),
  unlockIdentity: async (passphrase) => {
    const unlocked = Identity.unlock(await sealedBlob(), passphrase);
    identity?.free();
    identity = unlocked;
    return [unlocked.peerId(), unlocked.nickname];
  },
  lock: () => serial(() => forget()),
  eraseDevice: () =>
    serial(async () => {
      forget();
      await clear(db, STORES);
      const root = await navigator.storage.getDirectory();
      await Promise.all(
        ["media", "downloads"].map((dir) => root.removeEntry(dir, { recursive: true }).catch(() => undefined)),
      );
    }),
  exportMnemonic: async (passphrase) => {
    const check = Identity.unlock(await sealedBlob(), passphrase);
    try {
      return check.mnemonic();
    } finally {
      check.free();
    }
  },
  connect: (gatewayUrl, relayUrl, anonymous) =>
    serial(async () => {
      gateway.stop();
      relay.stop();
      const c = await startClient();
      await run({ type: "set_anonymity", require_onion: anonymous });
      await run({ type: "set_profile_name", name: requireIdentity().nickname });
      gateway.start(gatewayUrl);
      relay.start(relayUrl);
      return c.peerId();
    }),
  setAnonymity: async (anonymous) => {
    await serial(() => run({ type: "set_anonymity", require_onion: anonymous }));
  },
  createLink: async () => {
    const reply = awaitReply((r) =>
      r.op === "link_created" ? r.value : r.op === "warning" ? new Error(r.value) : undefined,
    );
    await serial(() => run({ type: "create_link" }));
    return reply;
  },
  joinLink: async (link) => {
    const trimmed = link.trim();
    const reply = awaitReply((r) =>
      r.op === "joined" ? r.value : r.op === "join_failed" && r.link === trimmed ? new Error(r.value) : undefined,
    );
    await serial(() => run({ type: "join_link", link: trimmed }));
    return reply;
  },
  command: (cmd) => serial(() => run(cmd)),
  sendFiles: (peer, files) =>
    serial(async () => {
      const sent = [];
      for (const file of files) {
        const { msgId, fileId } = await run({
          type: "send_file",
          peer,
          name: file.name,
          mime: file.type || "application/octet-stream",
          size: file.size,
          kind: "file",
        });
        if (!msgId || !fileId) continue;
        sources.set(fileId, file);
        await put(db, "files", fileId, file);
        sent.push({ msg_id: msgId, file_id: fileId, file_name: file.name, total_size: file.size });
      }
      return sent;
    }),
  fileSaved: async (fileId) =>
    (await sinkDir(false))
      .getFileHandle(fileId)
      .then(() => true)
      .catch(() => false),
  savedBlob: async (fileId) => (await (await sinkDir(false)).getFileHandle(fileId)).getFile(),
  qr: (text) => Promise.resolve(`data:image/svg+xml;charset=utf-8,${encodeURIComponent(qrSvg(text))}`),
  safetyNumber: (peer) => Promise.resolve(requireIdentity().safetyNumber(peer)),
  reconnect: () => {
    gateway.resume();
    return Promise.resolve();
  },
  conversations: async () => {
    const c = requireClient();
    const peers = await scan(db, "peers");
    const out = await Promise.all(
      peers.map(async ([key, value]) => {
        const peer_id = hex(key);
        const recent = (await history(peer_id, UNREAD_WINDOW)) as StoredView[];
        const last = recent[0] ?? null;
        return {
          peer_id,
          alias: c.contactAlias(key, value) ?? null,
          name: c.contactName(key, value) ?? null,
          request: (c.contactFlags(key, value) & 1) !== 0,
          blocked: (c.contactFlags(key, value) & 2) !== 0,
          last_message_at: last?.timestamp ?? 0,
          last,
          unread: recent.filter((m) => !m.outgoing && m.status !== "read").length,
        };
      }),
    );
    return out.sort((a, b) => b.last_message_at - a.last_message_at);
  },
  forgetPeer: (peer) =>
    serial(async () => {
      await run({ type: "remove_peer", peer });
      const [from, to] = historyRange(peer, undefined);
      await deleteRows(await scan(db, "messages", IDBKeyRange.bound(from, to, false, true)));
    }),
  deleteMessage: (peer, msgId, timestamp) =>
    serial(async () => {
      const [, from] = historyRange(peer, Math.max(0, timestamp - DELETE_WINDOW_MS));
      const [, to] = historyRange(peer, timestamp + DELETE_WINDOW_MS);
      const id = unhex(msgId);
      const rows = await scan(db, "messages", IDBKeyRange.bound(from, to));
      const row = rows.find(([key]) => key.subarray(key.length - 16).every((b, i) => b === id[i]));
      if (!row) throw new Error("NotFound");
      await run({ type: "discard_outgoing", msg_id: msgId });
      const msg = requireClient().openMessage(row[0], row[1], undefined) as StoredView;
      if (msg.file) await run({ type: "cancel_transfer", file_id: msg.file.file_id });
      await deleteRows([row]);
    }),
  history: (peer, limit, before) => history(peer, Math.min(limit, 500), before),
  sendMedia,
  mediaBlob,
  clearHistory: async () => {
    await clear(db, ["messages", "message_status", "media"]);
    const root = await navigator.storage.getDirectory();
    await Promise.all(
      ["media", "downloads"].map((dir) => root.removeEntry(dir, { recursive: true }).catch(() => undefined)),
    );
  },
};

self.onmessage = async ({ data: { id, method, args } }: MessageEvent<Request>) => {
  try {
    db = await ready;
    const handler = handlers[method] as (...a: unknown[]) => Promise<unknown>;
    post({ id, ok: true, result: await handler(...args) });
  } catch (err) {
    post({ id, ok: false, error: err instanceof Error ? err.message : String(err) });
  }
};

setInterval(() => void feed((c, now) => c.tick(now)), TICK_MS);
