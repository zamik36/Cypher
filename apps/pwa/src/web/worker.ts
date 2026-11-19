/// <reference lib="webworker" />
import init, {
  Client,
  conversationRange,
  historyRange,
  Identity,
  messageId,
  messageWindow,
  Provision,
  qrSvg,
  unreadWindow,
  waveformFromRms,
  type Effect,
  type ReadChunk,
  type Reply,
  type Row,
  type SealedIdentity,
} from "../wasm/cypher_wasm.js";
import { applyOps, clear, get, keys, messageRows, openDb, put, remove, scan, STORES } from "./idb";
import { clearHistory } from "./clearing";
import { deleteRows as deleteStoredRows } from "./deletion";
import { applyEffects, EffectApplier, type EffectSinks, type Sink } from "./effects";
import { buildHistory, importHistory, type HistoryDirs } from "./history";
import { offerFile, Refused, staleFiles, type Offering } from "./outgoing";
import { failed, Replies } from "./replies";
import type { Method, Methods, Request, WorkerMessage } from "./protocol";
import { CommandError, type IdentityKind, type PlainKind, toCmdErrorShape, toCommandError } from "@cypher/ui/errors";

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
const openSinks = new Map<string, { handle: FileSystemSyncAccessHandle; name: string; sink: Sink }>();
/** Commands waiting for their answer, by the id they went with. */
const replies = new Replies(REPLY_TIMEOUT_MS);

const post = (msg: WorkerMessage) => self.postMessage(msg);

/** A failure of `kind` for the UI, in the shape the desktop's commands fail with. */
const failure = (kind: PlainKind, message: string) => new CommandError({ kind, message });
const identityFailure = (reason: IdentityKind, message: string) =>
  new CommandError({ kind: "identity", reason, message });

/** Runs core interactions one at a time so effects apply in order. */
function serial<T>(task: () => Promise<T> | T): Promise<T> {
  const next = queue.then(task, task);
  queue = next.catch(() => undefined);
  return next;
}

/** Hands the core an input, unless it stopped after a storage failure. */
function feed(call: (c: Client, now: number) => Effect[]): Promise<void> {
  return serial(async () => {
    if (client && !applier.stopped) await apply(call(client, Date.now()));
  });
}

function requireIdentity(): Identity {
  if (!identity) throw failure("locked", "identity is locked");
  return identity;
}

function requireClient(): Client {
  if (!client) throw failure("not_connected", "not connected");
  return client;
}

const sinks: EffectSinks = {
  persist: (ops) => applyOps(db, ops),
  transmit: (data) => gateway.send(data),
  anonymous: (data) => relay.send(data),
  event: onEvent,
  reply: (r) => {
    replies.answer(r);
  },
  readChunk: (request) => void readChunk(request),
  openSink: (fileId, len, sink) => openSink(fileId, len, sink).catch((e: unknown) => sinkFailed(fileId, e)),
  writeChunk: (fileId, offset, data) => {
    try {
      openSinks.get(fileId)?.handle.write(data, { at: offset });
    } catch (e) {
      sinkFailed(fileId, e);
    }
  },
  closeSink: (fileId, complete) => closeSink(fileId, complete).catch((e: unknown) => sinkFailed(fileId, e)),
  buildHistory: (fileId, partial) => void runBuild(fileId, partial),
  importHistory: (fileId) => void runImport(fileId),
  disconnect: (reconnect) => gateway.restart(reconnect),
};

/**
 * A transfer's file could not be written: the transfer is cancelled, as the
 * native client does, instead of the rest of the effects being dropped.
 */
function sinkFailed(fileId: string, e: unknown) {
  console.warn("writing a transfer failed", fileId, e);
  void feed((c, now) => c.command({ type: "cancel_transfer", file_id: fileId }, now).effects);
}

/**
 * Storage failed under the core: it is ahead of what IndexedDB holds, so it
 * gets no input any more and nothing more leaves the device, as with the
 * native driver. Connecting again restores a core from what is stored.
 */
function storageFailed() {
  gateway.stop();
  relay.stop();
  replies.failAll(failed("storage_failed"));
  onEvent("error", "storage_failed");
  onEvent("disconnected", null);
}

/** Applies the current core's effects; replaced along with the core. */
let applier = new EffectApplier(sinks, storageFailed);
const apply = (effects: Effect[]) => applier.apply(effects);

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
      // Kept bytes of a file sent, read or not: the transfer is over.
      sources.delete(fileId);
      void remove(db, "files", fileId);
      break;
    }
    case "history_done":
    case "history_failed": {
      // The archive handed over (or given up) is no longer needed; one
      // received is deleted once taken in.
      const { file_id, outgoing } = payload as { file_id: string; outgoing: boolean };
      if (outgoing) void dropArchive(file_id);
      break;
    }
  }
  post({ event: channel, payload });
}

/**
 * Runs `cmd` under a fresh id and resolves with the value of the reply that
 * answers it, when that reply is `ok`; any other reply is why it failed.
 * Replies to other commands, and warnings, do not touch it.
 */
function request(cmd: Record<string, unknown>, ok: Reply["op"]): Promise<string> {
  return replies.request((id) => serial(() => run({ ...cmd, id })), ok);
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

  /** Detaches silently, until the next `start`: the caller resets the core itself. */
  stop() {
    clearTimeout(this.timer);
    this.url = "";
    this.lastUrl = "";
    const ws = this.ws;
    this.ws = null;
    ws?.close();
  }

  /** Closes the socket at the core's request, reconnecting if asked. */
  restart(reconnect: boolean) {
    if (!reconnect) this.url = "";
    this.ws?.close();
  }

  /** Connects again after `restart(false)`; does nothing while connected or after `stop`. */
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

/** This new device, waiting to be linked over its own gateway connection. */
let provision: Provision | null = null;
let linkTicker: ReturnType<typeof setInterval> | undefined;
/** Settles once the hand-over arrived, or waiting ended without it. */
let linkDone: { promise: Promise<void>; settle: (failure?: CommandError) => void } | null = null;

function feedLink(call: (p: Provision, now: number) => Effect[]): Promise<void> {
  return serial(async () => {
    if (provision) await applyEffects(call(provision, Date.now()), linkSinks);
  });
}

const linkSocket = new Link(
  () => void feedLink((p, now) => p.connected(now)),
  (data) => void feedLink((p, now) => p.frame(data, now)),
  () => undefined,
);

const linkSinks: EffectSinks = {
  persist: () => Promise.resolve(),
  transmit: (data) => linkSocket.send(data),
  anonymous: () => undefined,
  event: (channel) => {
    if (channel === "linked_here") linkDone?.settle();
    else if (channel === "link_failed") linkDone?.settle(failure("expired", "the offer expired"));
  },
  reply: () => undefined,
  readChunk: () => undefined,
  openSink: () => Promise.resolve(),
  writeChunk: () => undefined,
  closeSink: () => Promise.resolve(),
  buildHistory: () => undefined,
  importHistory: () => undefined,
  disconnect: () => linkSocket.stop(),
};

function stopLinking() {
  clearInterval(linkTicker);
  linkSocket.stop();
  linkDone?.settle(failure("cancelled", "cancelled"));
}

const relay = new Link(
  () => void feed((c, now) => c.anonymousChannel(true, now)),
  (data) => void feed((c, now) => c.anonymousFrame(data, now)),
  () => void feed((c, now) => c.anonymousChannel(false, now)),
);

async function readChunk(e: ReadChunk) {
  const file = sources.get(e.file_id) ?? (await get<File>(db, "files", e.file_id)) ?? (await archive(e.file_id));
  if (!file) return feed((c, now) => c.chunkUnavailable(e.file_id, now));
  sources.set(e.file_id, file);
  const data = new Uint8Array(await file.slice(e.offset, e.offset + e.len).arrayBuffer());
  const buf = new Uint8Array(e.headroom + data.length);
  buf.set(data, e.headroom);
  return feed((c, now) => c.chunkRead(e.file_id, e.index, buf, now));
}

/** A history archive built here, to send. */
async function archive(fileId: string): Promise<File | undefined> {
  return (await sinkDir("history"))
    .getFileHandle(fileId)
    .then((h) => h.getFile())
    .catch(() => undefined);
}

async function dropArchive(fileId: string) {
  sources.delete(fileId);
  await (await sinkDir("history")).removeEntry(fileId).catch(() => undefined);
}

async function historyDirs(): Promise<HistoryDirs> {
  return { history: await sinkDir("history"), media: await sinkDir("media") };
}

/** Builds the history archive the core asked for, then tells it. */
async function runBuild(fileId: string, partial: string[]) {
  let size: number | undefined;
  try {
    size = await buildHistory(db, requireClient(), await historyDirs(), fileId, partial);
  } catch (e) {
    console.warn("building the message history failed", e);
    await dropArchive(fileId);
  }
  await feed((c, now) => c.historyBuilt(fileId, size, now));
}

/** Takes in a received history archive, tells the core, then deletes it. */
async function runImport(fileId: string) {
  let ok = false;
  try {
    await importHistory(db, requireClient(), await historyDirs(), fileId);
    ok = true;
  } catch (e) {
    console.warn("taking in the message history failed", e);
  }
  // Deleted only once the core knows: closed in between, it is taken in again.
  await feed((c, now) => c.historyImported(fileId, ok, now));
  await dropArchive(fileId);
}

/** The OPFS directory each kind of transfer is kept in. */
const SINK_DIRS: Record<Sink, string> = { download: "downloads", media: "media", history: "history" };

async function sinkDir(sink: Sink) {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(SINK_DIRS[sink], { create: true });
}

async function openSink(fileId: string, len: number, sink: Sink) {
  if (openSinks.has(fileId)) return;
  const file = await (await sinkDir(sink)).getFileHandle(fileId, { create: true });
  const handle = await file.createSyncAccessHandle();
  if (handle.getSize() !== len) handle.truncate(len);
  openSinks.set(fileId, { handle, name: offerNames.get(fileId) ?? fileId, sink });
}

async function closeSink(fileId: string, complete: boolean) {
  const sink = openSinks.get(fileId);
  if (!sink) return;
  openSinks.delete(fileId);
  offerNames.delete(fileId);
  sink.handle.flush();
  sink.handle.close();
  const dir = await sinkDir(sink.sink);
  if (!complete) {
    await dir.removeEntry(fileId).catch(() => undefined);
  } else if (sink.sink === "download") {
    const blob = await (await dir.getFileHandle(fileId)).getFile();
    post({ download: { fileId, name: sink.name, blob } });
  }
}

async function sealedBlob(): Promise<Uint8Array> {
  const blob = await get<Uint8Array>(db, "identity", IDENTITY_KEY);
  if (!blob) throw identityFailure("missing", "no identity");
  return blob;
}

async function adopt(sealed: SealedIdentity): Promise<string> {
  if ((await get<Uint8Array>(db, "identity", IDENTITY_KEY)) !== undefined)
    throw identityFailure("exists", "an identity already exists");
  await put(db, "identity", IDENTITY_KEY, sealed.blob);
  identity?.free();
  identity = sealed.intoIdentity();
  return identity.peerId();
}

async function startClient(): Promise<Client> {
  if (!identity) throw failure("locked", "identity is locked");
  const [meta, peers, sessions, outbox, transfers] = await Promise.all([
    scan(db, "meta"),
    scan(db, "peers"),
    scan(db, "sessions"),
    scan(db, "outbox"),
    scan(db, "transfers"),
  ]);
  // Bytes kept for a file whose offer never became a transfer, or whose
  // end went unrecorded.
  const stale = staleFiles(
    (await keys(db, "files")).filter((k) => typeof k === "string"),
    transfers,
  );
  await Promise.all(stale.map((id) => remove(db, "files", id)));
  client?.free();
  client = new Client(identity, { meta, peers, sessions, outbox, transfers }, Date.now());
  applier = new EffectApplier(sinks, storageFailed);
  await apply(client.startupEffects());
  return client;
}

async function run(cmd: Record<string, unknown>) {
  if (applier.stopped) throw failed("storage_failed");
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
    const cmd = {
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
    };
    const ids = await offerFile(offering, cmd, inline ? undefined : new File([blob], name, { type: mime }));
    if (!ids) throw failure("internal", "media rejected");
    return { msg_id: ids.msgId, file_id: ids.fileId, duration_ms: durationMs, ...(waveform && { waveform }) };
  });
}

async function mediaBlob(fileId: string): Promise<Blob> {
  const record = await get<Uint8Array>(db, "media", unhex(fileId));
  if (!record) throw failure("not_found", "media not found");
  const file = await (await (await sinkDir("media")).getFileHandle(fileId)).getFile();
  const sealed = new Uint8Array(await file.arrayBuffer());
  const { mime, bytes } = requireClient().openMedia(fileId, record, sealed);
  return new Blob([bytes], { type: mime });
}

/** Every stored message with `peer`, whatever its date. */
function conversation(peer: string): IDBKeyRange {
  const [first, last] = conversationRange(peer);
  return IDBKeyRange.bound(first, last);
}

async function history(peer: string, limit: number, before?: number) {
  const c = requireClient();
  const range =
    before === undefined ? conversation(peer) : IDBKeyRange.bound(...historyRange(peer, before), false, true);
  const [rows = []] = await messageRows(db, [range], limit, messageId);
  return rows.map(([key, value, status]) => c.openMessage(key, value, status));
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
  // Their answers would come from a client that is gone.
  replies.failAll(failure("locked", "identity is locked"));
}

/** How far around the UI's time a deleted message is looked for. */
const DELETE_WINDOW_MS = 60_000;

/** Deletes stored messages as the core says each one goes. */
function deleteRows(rows: Row[]) {
  const c = requireClient();
  return deleteStoredRows(rows, {
    deletion: (key, value) => c.messageDeletion(key, value),
    apply: (ops) => applyOps(db, ops),
    removeNote: async (fileId) => (await sinkDir("media")).removeEntry(fileId).catch(() => undefined),
  });
}

/** Offers a file through the core, keeping its bytes as the transfer's source. */
const offering: Offering = {
  run: async (cmd) => {
    let outcome: ReturnType<Client["command"]>;
    try {
      if (applier.stopped) throw failed("storage_failed");
      outcome = requireClient().command(cmd, Date.now());
    } catch (e) {
      throw new Refused(toCommandError(e));
    }
    const { effects, ...ids } = outcome;
    await apply(effects);
    return ids;
  },
  keep: async (fileId, file) => {
    await put(db, "files", fileId, file);
    sources.set(fileId, file);
  },
  drop: async (fileId) => {
    sources.delete(fileId);
    await remove(db, "files", fileId);
  },
};

/** The fields of a decrypted message the worker itself reads. */
interface StoredView {
  file: { file_id: string } | null;
}

type Handlers = { [M in Method]: (...args: Parameters<Methods[M]>) => Promise<ReturnType<Methods[M]>> };

const handlers: Handlers = {
  startLink: (gatewayUrl, name) => {
    stopLinking();
    provision?.free();
    provision = new Provision(name, Date.now());
    let settle: (failure?: CommandError) => void = () => undefined;
    const promise = new Promise<void>((resolve, reject) => {
      settle = (failure) => (failure ? reject(failure) : resolve());
    });
    // Waiting may end with nobody listening yet.
    promise.catch(() => undefined);
    linkDone = { promise, settle };
    linkSocket.start(gatewayUrl);
    linkTicker = setInterval(() => void feedLink((p, now) => p.tick(now)), TICK_MS);
    return Promise.resolve(provision.offer());
  },
  finishLink: async (passphrase) => {
    if (!provision || !linkDone) throw failure("not_linking", "not waiting to be linked");
    await linkDone.promise;
    clearInterval(linkTicker);
    const sealed = provision.finish(passphrase);
    provision.free();
    provision = null;
    linkDone = null;
    const peer = await adopt(sealed);
    return [peer, requireIdentity().nickname];
  },
  cancelLink: () => {
    stopLinking();
    provision?.free();
    provision = null;
    return Promise.resolve();
  },
  linkDevice: async (offer, history) => {
    await request({ type: "link_device", offer, history }, "device_linked");
  },
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
      await run({ type: "set_server", addr: gatewayUrl });
      await run({ type: "set_anonymity", require_onion: anonymous });
      await run({ type: "set_profile_name", name: requireIdentity().nickname });
      gateway.start(gatewayUrl);
      relay.start(relayUrl);
      return c.peerId();
    }),
  setAnonymity: async (anonymous) => {
    await serial(() => run({ type: "set_anonymity", require_onion: anonymous }));
  },
  createLink: () => request({ type: "create_link" }, "link_created"),
  joinLink: (link) => request({ type: "join_link", link: link.trim() }, "joined"),
  command: (cmd) => serial(() => run(cmd)),
  sendFiles: (peer, files) =>
    serial(async () => {
      const sent = [];
      for (const file of files) {
        const cmd = {
          type: "send_file",
          peer,
          name: file.name,
          mime: file.type || "application/octet-stream",
          size: file.size,
          kind: "file",
        };
        const ids = await offerFile(offering, cmd, file);
        if (!ids) continue;
        sent.push({ msg_id: ids.msgId, file_id: ids.fileId, file_name: file.name, total_size: file.size });
      }
      return sent;
    }),
  fileSaved: async (fileId) =>
    (await sinkDir("download"))
      .getFileHandle(fileId)
      .then(() => true)
      .catch(() => false),
  savedBlob: async (fileId) => (await (await sinkDir("download")).getFileHandle(fileId)).getFile(),
  qr: (text) => Promise.resolve(`data:image/svg+xml;charset=utf-8,${encodeURIComponent(qrSvg(text))}`),
  safetyNumber: (peer) => Promise.resolve(requireIdentity().safetyNumber(peer)),
  reconnect: () => {
    gateway.resume();
    return Promise.resolve();
  },
  conversations: async () => {
    const c = requireClient();
    const peers = await scan(db, "peers");
    const ranges = peers.map(([key]) => conversation(hex(key)));
    return c.chatList(peers, await messageRows(db, ranges, unreadWindow(), messageId));
  },
  forgetPeer: (peer) =>
    serial(async () => {
      await run({ type: "remove_peer", peer });
      await deleteRows(await scan(db, "messages", conversation(peer)));
    }),
  deleteMessage: (peer, msgId, timestamp) =>
    serial(async () => {
      const rows = await scan(db, "messages", IDBKeyRange.bound(...messageWindow(peer, timestamp, DELETE_WINDOW_MS)));
      const row = rows.find(([key]) => {
        const id = messageId(key);
        return id !== undefined && hex(id) === msgId;
      });
      if (!row) throw failure("not_found", "no such message");
      await run({ type: "discard_outgoing", msg_id: msgId });
      const msg = requireClient().openMessage(row[0], row[1], undefined) as StoredView;
      if (msg.file) await run({ type: "cancel_transfer", file_id: msg.file.file_id });
      await deleteRows([row]);
    }),
  history: (peer, limit, before) => history(peer, Math.min(limit, 500), before),
  sendMedia,
  mediaBlob,
  // In the queue: no sink opens between the cancel and the emptying.
  clearHistory: () =>
    serial(() =>
      clearHistory({
        cancelWriting: async () => {
          const writing = [...openSinks].filter(([, s]) => s.sink !== "history").map(([fileId]) => fileId);
          for (const fileId of writing) await run({ type: "cancel_transfer", file_id: fileId });
        },
        clearRows: () => clear(db, ["messages", "message_status", "media"]),
        root: () => navigator.storage.getDirectory(),
      }),
    ),
};

self.onmessage = async ({ data: { id, method, args } }: MessageEvent<Request>) => {
  try {
    db = await ready;
    const handler = handlers[method] as (...a: unknown[]) => Promise<unknown>;
    post({ id, ok: true, result: await handler(...args) });
  } catch (err) {
    post({ id, ok: false, error: toCmdErrorShape(err) });
  }
};

setInterval(() => void feed((c, now) => c.tick(now)), TICK_MS);
