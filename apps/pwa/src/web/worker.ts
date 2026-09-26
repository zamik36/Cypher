/// <reference lib="webworker" />
import init, { Client, Identity, qrSvg, type SealedIdentity } from "../wasm/cypher_wasm.js";
import { applyOps, clear, get, openDb, put, remove, scan, type Op } from "./idb";
import type { Method, Methods, Request, WorkerMessage } from "./protocol";

declare const self: DedicatedWorkerGlobalScope;

type Effect =
  | { kind: "transmit" | "anonymous"; data: Uint8Array }
  | Op
  | { kind: "event"; channel: string; payload: unknown }
  | { kind: "reply"; op: string; value: string; link?: string }
  | { kind: "read_chunk"; file_id: string; index: number; offset: number; len: number; headroom: number }
  | { kind: "open_sink"; file_id: string; len: number; sealed: boolean }
  | { kind: "write_chunk"; file_id: string; offset: number; data: Uint8Array }
  | { kind: "close_sink"; file_id: string; complete: boolean }
  | { kind: "disconnect"; reconnect: boolean };

type Reply = Extract<Effect, { kind: "reply" }>;

const MIN_BACKOFF_MS = 1000;
const MAX_BACKOFF_MS = 30_000;
const TICK_MS = 500;
const REPLY_TIMEOUT_MS = 20_000;
const IDENTITY_KEY = "current";

const ready = init().then(() => openDb("cypher"));
let db: IDBDatabase;
let identity: Identity | null = null;
let client: Client | null = null;
let queue: Promise<unknown> = Promise.resolve();

const sources = new Map<string, File>();
const offerNames = new Map<string, string>();
const sinks = new Map<string, { handle: FileSystemSyncAccessHandle; name: string; sealed: boolean }>();
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

function requireClient(): Client {
  if (!client) throw new Error("not connected");
  return client;
}

/** Persists before anything leaves the device, mirroring the native driver. */
async function apply(effects: Effect[]): Promise<void> {
  let batch: Op[] = [];
  const flush = async () => {
    if (batch.length === 0) return;
    const ops = batch;
    batch = [];
    await applyOps(db, ops);
  };
  for (const e of effects) {
    switch (e.kind) {
      case "put":
      case "delete":
        batch.push(e);
        break;
      case "transmit":
        await flush();
        gateway.send(e.data);
        break;
      case "anonymous":
        await flush();
        relay.send(e.data);
        break;
      case "event":
        onEvent(e.channel, e.payload);
        break;
      case "reply":
        for (const w of waiters) if (w(e)) waiters.delete(w);
        break;
      case "read_chunk":
        void readChunk(e);
        break;
      case "open_sink":
        await openSink(e.file_id, e.len, e.sealed);
        break;
      case "write_chunk":
        sinks.get(e.file_id)?.handle.write(e.data, { at: e.offset });
        break;
      case "close_sink":
        await closeSink(e.file_id, e.complete);
        break;
      case "disconnect":
        gateway.restart(e.reconnect);
        break;
    }
  }
  await flush();
}

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
      this.timer = setTimeout(() => this.connect(), this.backoff);
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

async function readChunk(e: Extract<Effect, { kind: "read_chunk" }>) {
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
  if (sinks.has(fileId)) return;
  const file = await (await sinkDir(sealed)).getFileHandle(fileId, { create: true });
  const handle = await file.createSyncAccessHandle();
  if (handle.getSize() !== len) handle.truncate(len);
  sinks.set(fileId, { handle, name: offerNames.get(fileId) ?? fileId, sealed });
}

async function closeSink(fileId: string, complete: boolean) {
  const sink = sinks.get(fileId);
  if (!sink) return;
  sinks.delete(fileId);
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
  if (await get(db, "identity", IDENTITY_KEY)) throw new Error("an identity already exists");
  await put(db, "identity", IDENTITY_KEY, sealed.blob);
  identity?.free();
  identity = sealed.intoIdentity();
  return identity.peerId();
}

async function startClient(): Promise<Client> {
  if (!identity) throw new Error("identity is locked");
  const [meta, peers, outbox, transfers] = await Promise.all(
    ["meta", "peers", "outbox", "transfers"].map((t) => scan(db, t)),
  );
  client?.free();
  client = new Client(identity, meta, peers, outbox, transfers, Date.now());
  await apply(client.startupEffects());
  return client;
}

async function run(cmd: Record<string, unknown>) {
  const out = requireClient().command(cmd, Date.now()) as { effects: Effect[]; msgId?: string; fileId?: string };
  await apply(out.effects);
  return { msgId: out.msgId, fileId: out.fileId };
}

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

async function history(peer: string, limit: number, before?: number) {
  const c = requireClient();
  const [from, to] = c.historyRange(peer, before) as [Uint8Array, Uint8Array];
  const rows = await scan(db, "messages", IDBKeyRange.bound(from, to, false, true), limit, true);
  const statuses = await Promise.all(
    rows.map(([key]) => get<Uint8Array>(db, "message_status", key.subarray(key.length - 16))),
  );
  return rows.map(([key, value], i) => c.openMessage(key, value, statuses[i]));
}

type Handlers = { [M in Method]: (...args: Parameters<Methods[M]>) => Promise<ReturnType<Methods[M]>> };

const handlers: Handlers = {
  hasIdentity: async () => Boolean(await get(db, "identity", IDENTITY_KEY)),
  createIdentity: (nickname, passphrase) => adopt(Identity.create(nickname, passphrase)),
  importMnemonic: (mnemonic, nickname, passphrase) => adopt(Identity.import(mnemonic, nickname, passphrase)),
  unlockIdentity: async (passphrase) => {
    const unlocked = Identity.unlock(await sealedBlob(), passphrase);
    identity?.free();
    identity = unlocked;
    return [unlocked.peerId(), unlocked.nickname];
  },
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
        const { fileId } = await run({
          type: "send_file",
          peer,
          name: file.name,
          mime: file.type || "application/octet-stream",
          size: file.size,
          kind: "file",
        });
        if (!fileId) continue;
        sources.set(fileId, file);
        await put(db, "files", fileId, file);
        sent.push({ file_id: fileId, file_name: file.name, total_size: file.size });
      }
      return sent;
    }),
  releaseDownload: async (fileId) => {
    await (await sinkDir(false)).removeEntry(fileId).catch(() => undefined);
  },
  qr: async (text) => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(qrSvg(text))}`,
  conversations: async () => {
    const peers = await scan(db, "peers");
    const out = await Promise.all(
      peers.map(async ([key]) => {
        const peer_id = hex(key);
        const [last] = (await history(peer_id, 1)) as { timestamp: number }[];
        return { peer_id, display_name: null, last_message_at: last?.timestamp ?? 0 };
      }),
    );
    return out.sort((a, b) => b.last_message_at - a.last_message_at);
  },
  history: (peer, limit, before) => history(peer, Math.min(limit, 500), before),
  clearHistory: () => clear(db, ["messages", "message_status"]),
};

self.onmessage = async ({ data: { id, method, args } }: MessageEvent<Request>) => {
  try {
    db ??= await ready;
    const handler = handlers[method] as (...a: unknown[]) => Promise<unknown>;
    post({ id, ok: true, result: await handler(...args) });
  } catch (err) {
    post({ id, ok: false, error: err instanceof Error ? err.message : String(err) });
  }
};

setInterval(() => void feed((c, now) => c.tick(now)), TICK_MS);
