/**
 * PWA API layer — communicates with the gateway via WebSocket using the
 * same binary protocol as the desktop Tauri client.
 */
import {
  encodeSessionInit, encodeSignalRequestPeer, encodeChatSend,
  encodeFileOffer, encodeFileChunk, encodeFileComplete,
  encodeFileChunkAck, dispatch as protoDispatch,
  hexEncode, hexDecode, randomBytes,
} from "./proto";


export interface LinkInfo { link_id: string }
export interface ChatMessage { from: string; text: string; timestamp: number }
export interface TransferInfo {
  file_id: string;
  file_name: string;
  total_size: number;
  progress: number;
  direction: string;
  status: "active" | "complete" | "error";
}


type Unsubscribe = () => void;

interface EventMap {
  connected: string;
  disconnected: undefined;
  peer_connected: string;
  message: ChatMessage;
  message_sent: { to: string; text: string; timestamp: number };
  file_offered: { from: string; file_id: string; name: string; size: number };
  file_progress: { file_id: string; progress: number };
  file_complete: string;
  error: string;
}

type EventCallback<T> = (payload: T) => void;

const listeners: { [K in keyof EventMap]?: EventCallback<EventMap[K]>[] } = {};

function emit<K extends keyof EventMap>(event: K, payload: EventMap[K]) {
  for (const cb of listeners[event] ?? []) cb(payload);
}

function on<K extends keyof EventMap>(event: K, cb: EventCallback<EventMap[K]>): Unsubscribe {
  if (!listeners[event]) listeners[event] = [];
  listeners[event]!.push(cb);
  return () => {
    listeners[event] = listeners[event]!.filter((x) => x !== cb) as typeof listeners[K];
  };
}


let ws: WebSocket | null = null;
let peerId: Uint8Array = randomBytes(32);
let peerIdHex = hexEncode(peerId);

/** Replace the peerId with a persistent one (called after identity unlock). */
export function setPeerId(newPeerId: Uint8Array) {
  peerId = newPeerId;
  peerIdHex = hexEncode(peerId);
}
let activePeerForTransfer: string | null = null;

let pendingLinkResolve: ((v: string) => void) | null = null;
let pendingLinkReject: ((e: Error) => void) | null = null;

// File receive buffers: fileId hex → chunks[] + creation timestamp for timeout.
const FILE_TRANSFER_TIMEOUT_MS = 5 * 60 * 1000; // 5 minutes
const fileBuffers = new Map<string, { name: string; size: number; totalChunks: number; chunks: Map<number, Uint8Array>; hash: Uint8Array; createdAt: number }>();

const CHUNK_SIZE = 64 * 1024; // 64 KB

function arraysEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) { if (a[i] !== b[i]) return false; }
  return true;
}

function sanitizeFilename(name: string): string {
  return name.replace(/[\/\\?%*:|"<>]/g, "_");
}

function send(data: Uint8Array) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(data);
  }
}

function handleMessage(data: ArrayBuffer) {
  const msg = protoDispatch(new Uint8Array(data));
  switch (msg.type) {
    case "SessionAck":
      emit("connected", peerIdHex);
      break;
    case "ChatSend": {
      const from = hexEncode(msg.msg.peerId);
      // For PWA-to-PWA: ciphertext is plaintext UTF-8.
      const MAX_MSG_LEN = 50_000;
      const raw = new TextDecoder().decode(msg.msg.ciphertext);
      const text = raw.length > MAX_MSG_LEN ? raw.slice(0, MAX_MSG_LEN) + "… [truncated]" : raw;
      emit("message", { from, text, timestamp: Date.now() } as ChatMessage);
      break;
    }
    case "FileOffer": {
      const from = hexEncode(msg.msg.peerId);
      const fileId = hexEncode(msg.msg.fileId);
      fileBuffers.set(fileId, {
        name: msg.msg.name,
        size: msg.msg.size,
        totalChunks: msg.msg.chunks,
        chunks: new Map(),
        hash: msg.msg.hash,
        createdAt: Date.now(),
      });
      // Auto-cleanup stale transfer after timeout.
      setTimeout(() => {
        if (fileBuffers.has(fileId)) {
          fileBuffers.delete(fileId);
          emit("error", `File transfer "${msg.msg.name}" timed out`);
        }
      }, FILE_TRANSFER_TIMEOUT_MS);
      emit("file_offered", { from, file_id: fileId, name: msg.msg.name, size: msg.msg.size });
      break;
    }
    case "FileChunk": {
      const fileId = hexEncode(msg.msg.fileId);
      const buf = fileBuffers.get(fileId);
      if (buf) {
        buf.chunks.set(msg.msg.index, msg.msg.data);
        const progress = buf.chunks.size / buf.totalChunks;
        emit("file_progress", { file_id: fileId, progress });

        // Send ack.
        send(encodeFileChunkAck(msg.msg.peerId, msg.msg.fileId, msg.msg.index));
      }
      break;
    }
    case "FileComplete": {
      const fileId = hexEncode(msg.msg.fileId);
      const buf = fileBuffers.get(fileId);
      if (buf) {
        // Assemble file.
        const parts: Uint8Array[] = [];
        let totalLen = 0;
        for (let i = 0; i < buf.totalChunks; i++) {
          const chunk = buf.chunks.get(i);
          if (chunk) { parts.push(chunk); totalLen += chunk.length; }
        }
        const assembled = new Uint8Array(totalLen);
        let offset = 0;
        for (const part of parts) { assembled.set(part, offset); offset += part.length; }

        // Verify SHA-256 hash integrity.
        crypto.subtle.digest("SHA-256", assembled).then((hashBuf) => {
          const hash = new Uint8Array(hashBuf);
          if (buf.hash.length > 0 && !arraysEqual(hash, buf.hash)) {
            emit("error", "File integrity check failed — hash mismatch");
            fileBuffers.delete(fileId);
            return;
          }
          // Hash OK — trigger download.
          const blob = new Blob([assembled]);
          const url = URL.createObjectURL(blob);
          const a = document.createElement("a");
          a.href = url;
          a.download = sanitizeFilename(buf.name);
          a.click();
          URL.revokeObjectURL(url);
          fileBuffers.delete(fileId);
          emit("file_complete", fileId);
        });
      }
      break;
    }
    case "Unknown": {
      // Could be a JSON response from signaling (e.g., link created, peer found).
      try {
        const text = new TextDecoder().decode(new Uint8Array(data));
        const json = JSON.parse(text);
        if (json.link_id && pendingLinkResolve) {
          pendingLinkResolve(json.link_id);
          pendingLinkResolve = null;
          pendingLinkReject = null;
        }
        if (json.found === true && json.peer_id) {
          emit("peer_connected", json.peer_id as string);
        }
        if (json.peer_joined === true && json.peer_id) {
          emit("peer_connected", json.peer_id as string);
        }
      } catch {
        // Not JSON, ignore.
      }
      break;
    }
    default:
      break;
  }
}


export const api = {
  connectToGateway(addr: string): Promise<void> {
    return new Promise((resolve, reject) => {
      // Determine WS URL: use wss:// when page is served over HTTPS.
      let wsUrl: string;
      if (addr.startsWith("ws")) {
        wsUrl = addr;
      } else {
        const protocol = location.protocol === "https:" ? "wss" : "ws";
        wsUrl = `${protocol}://${addr}`;
      }
      try { if (ws) ws.close(); } catch { /* ignore */ }

      let settled = false;
      ws = new WebSocket(wsUrl);
      ws.binaryType = "arraybuffer";

      ws.onopen = () => {
        // Send SESSION_INIT — don't resolve until SessionAck arrives.
        const nonce = randomBytes(32);
        send(encodeSessionInit(peerId, nonce));
      };

      ws.onmessage = (e: MessageEvent) => {
        if (e.data instanceof ArrayBuffer) {
          // Resolve on first SessionAck (gateway confirmed our session).
          if (!settled) {
            const msg = protoDispatch(new Uint8Array(e.data));
            if (msg.type === "SessionAck") {
              settled = true;
              resolve();
            }
          }
          handleMessage(e.data);
        }
      };

      ws.onclose = () => {
        if (!settled) { settled = true; reject(new Error("Connection closed before SessionAck")); }
        emit("disconnected", undefined);
      };

      ws.onerror = () => {
        if (!settled) { settled = true; reject(new Error("WebSocket connection failed")); }
      };

      // Timeout: don't wait forever for SessionAck.
      setTimeout(() => {
        if (!settled) { settled = true; reject(new Error("SessionAck timeout")); try { ws?.close(); } catch { /* ignore */ } }
      }, 10_000);
    });
  },

  createLink(): Promise<LinkInfo> {
    return new Promise((resolve, reject) => {
      pendingLinkResolve = (linkId: string) => resolve({ link_id: linkId });
      pendingLinkReject = reject;
      // Send JSON action (same as desktop client).
      const msg = JSON.stringify({ action: "create_link", peer_id: peerIdHex });
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(new TextEncoder().encode(msg));
      }
      // Timeout after 10s.
      setTimeout(() => {
        if (pendingLinkReject) {
          pendingLinkReject(new Error("create_link timeout"));
          pendingLinkResolve = null;
          pendingLinkReject = null;
        }
      }, 10000);
    });
  },

  joinLink(linkId: string): Promise<string> {
    return new Promise((resolve, reject) => {
      // Listen for peer_connected event once.
      const unsub = on("peer_connected", (remotePeerId) => {
        unsub();
        resolve(remotePeerId);
      });
      send(encodeSignalRequestPeer(linkId));
      setTimeout(() => { unsub(); reject(new Error("join_link timeout")); }, 15000);
    });
  },

  sendMessage(targetPeerId: string, text: string): Promise<void> {
    // PWA-to-PWA: send plaintext in ciphertext field.
    const ciphertext = new TextEncoder().encode(text);
    send(encodeChatSend(hexDecode(targetPeerId), ciphertext, new Uint8Array(0), 0));
    // Notify listeners so App.tsx can persist the sent message.
    emit("message_sent", { to: targetPeerId, text, timestamp: Date.now() });
    return Promise.resolve();
  },

  getMessages(): Promise<ChatMessage[]> {
    return Promise.resolve([]);
  },

  sendFile(file: File): Promise<TransferInfo> {
    return new Promise((resolve, reject) => {
      const activePeer = activePeerForTransfer;
      if (!activePeer) {
        reject(new Error("No active peer to send to"));
        return;
      }
      const targetPeerId = hexDecode(activePeer);
      const fileId = randomBytes(16);
      const fileIdHex = hexEncode(fileId);
      const totalChunks = Math.ceil(file.size / CHUNK_SIZE) || 1;

      // Compute SHA-256 hash and send.
      file.arrayBuffer().then(async (arrayBuf) => {
        const hashBuf = await crypto.subtle.digest("SHA-256", arrayBuf);
        const hash = new Uint8Array(hashBuf);
        const fileData = new Uint8Array(arrayBuf);

        // Send FileOffer.
        send(encodeFileOffer(targetPeerId, fileId, file.name, file.size, totalChunks, hash, 0));

        const info: TransferInfo = {
          file_id: fileIdHex,
          file_name: file.name,
          total_size: file.size,
          progress: 0,
          direction: "send",
          status: "active",
        };
        resolve(info);

        // Send chunks.
        for (let i = 0; i < totalChunks; i++) {
          const start = i * CHUNK_SIZE;
          const end = Math.min(start + CHUNK_SIZE, file.size);
          const chunk = fileData.slice(start, end);
          const chunkHash = new Uint8Array(await crypto.subtle.digest("SHA-256", chunk));
          send(encodeFileChunk(targetPeerId, fileId, i, chunk, chunkHash, new Uint8Array(0), 0));

          emit("file_progress", { file_id: fileIdHex, progress: (i + 1) / totalChunks });

          // Small yield to avoid blocking UI.
          if (i % 10 === 9) await new Promise((r) => setTimeout(r, 0));
        }

        // Send FileComplete.
        send(encodeFileComplete(targetPeerId, fileId));
        emit("file_complete", fileIdHex);
      }).catch(reject);
    });
  },

  acceptFile(_fileId: string, _destPath: string): Promise<void> {
    // In PWA, auto-accept is handled by handleMessage — file chunks are
    // buffered automatically and the file is downloaded on completion.
    // In PWA, accept is implicit — chunks are buffered automatically
    // and the file is downloaded on completion.
    return Promise.resolve();
  },

  getTransfers(): Promise<TransferInfo[]> {
    return Promise.resolve([]);
  },

  generateQr(_linkId: string): Promise<string> {
    // QR generation is optional — could add a JS QR library later.
    return Promise.resolve("");
  },
};


export function onConnected(cb: (peerId: string) => void): Promise<Unsubscribe> {
  return Promise.resolve(on("connected", cb));
}
export function onDisconnected(cb: () => void): Promise<Unsubscribe> {
  return Promise.resolve(on("disconnected", cb as EventCallback<undefined>));
}
export function onPeerConnected(cb: (peerId: string) => void): Promise<Unsubscribe> {
  return Promise.resolve(on("peer_connected", cb));
}
export function onMessage(cb: (msg: ChatMessage) => void): Promise<Unsubscribe> {
  return Promise.resolve(on("message", cb));
}
export function onFileOffered(cb: (info: { from: string; file_id: string; name: string; size: number }) => void): Promise<Unsubscribe> {
  return Promise.resolve(on("file_offered", cb));
}
export function onFileProgress(cb: (info: { file_id: string; progress: number }) => void): Promise<Unsubscribe> {
  return Promise.resolve(on("file_progress", cb));
}
export function onFileComplete(cb: (fileId: string) => void): Promise<Unsubscribe> {
  return Promise.resolve(on("file_complete", cb));
}
export function onError(cb: (msg: string) => void): Promise<Unsubscribe> {
  return Promise.resolve(on("error", cb));
}
export function onMessageSent(cb: (info: { to: string; text: string; timestamp: number }) => void): Promise<Unsubscribe> {
  return Promise.resolve(on("message_sent", cb));
}

/** Set the active peer for file sending. Called from stores. */
export function setActivePeerForTransfer(peerId: string | null) {
  activePeerForTransfer = peerId;
}
