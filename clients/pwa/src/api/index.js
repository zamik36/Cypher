/**
 * PWA API layer — communicates with the gateway via WebSocket using the
 * same binary protocol as the desktop Tauri client.
 */
import { encodeSessionInit, encodeSignalRequestPeer, encodeChatSend, encodeFileOffer, encodeFileChunk, encodeFileComplete, encodeFileChunkAck, dispatch as protoDispatch, hexEncode, hexDecode, randomBytes, } from "./proto";
const listeners = {};
function emit(event, payload) {
    for (const cb of listeners[event] ?? [])
        cb(payload);
}
function on(event, cb) {
    if (!listeners[event])
        listeners[event] = [];
    listeners[event].push(cb);
    return () => {
        listeners[event] = listeners[event].filter((x) => x !== cb);
    };
}
let ws = null;
let peerId = randomBytes(32);
let peerIdHex = hexEncode(peerId);
let pendingLinkResolve = null;
let pendingLinkReject = null;
// File receive buffers: fileId hex → chunks[]
const fileBuffers = new Map();
const CHUNK_SIZE = 64 * 1024; // 64 KB
function send(data) {
    if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(data);
    }
}
function handleMessage(data) {
    const msg = protoDispatch(new Uint8Array(data));
    switch (msg.type) {
        case "SessionAck":
            emit("connected", peerIdHex);
            break;
        case "ChatSend": {
            const from = hexEncode(msg.msg.peerId);
            // For PWA-to-PWA: ciphertext is plaintext UTF-8.
            const text = new TextDecoder().decode(msg.msg.ciphertext);
            emit("message", { from, text, timestamp: Date.now() });
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
            });
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
                // Assemble file and trigger download.
                const parts = [];
                for (let i = 0; i < buf.totalChunks; i++) {
                    const chunk = buf.chunks.get(i);
                    if (chunk)
                        parts.push(chunk);
                }
                const blob = new Blob(parts);
                const url = URL.createObjectURL(blob);
                const a = document.createElement("a");
                a.href = url;
                a.download = buf.name;
                a.click();
                URL.revokeObjectURL(url);
                fileBuffers.delete(fileId);
                emit("file_complete", fileId);
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
                    emit("peer_connected", json.peer_id);
                }
                if (json.peer_joined === true && json.peer_id) {
                    emit("peer_connected", json.peer_id);
                }
            }
            catch {
                // Not JSON, ignore.
            }
            break;
        }
        default:
            break;
    }
}
export const api = {
    connectToGateway(addr) {
        return new Promise((resolve, reject) => {
            // Determine WS URL: if addr is host:port, use ws://host:port.
            const wsUrl = addr.startsWith("ws") ? addr : `ws://${addr}`;
            try {
                if (ws)
                    ws.close();
            }
            catch { /* ignore */ }
            ws = new WebSocket(wsUrl);
            ws.binaryType = "arraybuffer";
            ws.onopen = () => {
                // Send SESSION_INIT immediately.
                const nonce = randomBytes(32);
                send(encodeSessionInit(peerId, nonce));
                resolve();
            };
            ws.onmessage = (e) => {
                if (e.data instanceof ArrayBuffer) {
                    handleMessage(e.data);
                }
            };
            ws.onclose = () => {
                emit("disconnected", undefined);
            };
            ws.onerror = () => {
                reject(new Error("WebSocket connection failed"));
            };
        });
    },
    createLink() {
        return new Promise((resolve, reject) => {
            pendingLinkResolve = (linkId) => resolve({ link_id: linkId });
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
    joinLink(linkId) {
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
    sendMessage(targetPeerId, text) {
        // PWA-to-PWA: send plaintext in ciphertext field.
        const ciphertext = new TextEncoder().encode(text);
        send(encodeChatSend(hexDecode(targetPeerId), ciphertext, new Uint8Array(0), 0));
        return Promise.resolve();
    },
    getMessages() {
        return Promise.resolve([]);
    },
    sendFile(file) {
        return new Promise((resolve, reject) => {
            const activePeer = globalThis.__p2pActivePeer;
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
                const info = {
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
                    if (i % 10 === 9)
                        await new Promise((r) => setTimeout(r, 0));
                }
                // Send FileComplete.
                send(encodeFileComplete(targetPeerId, fileId));
                emit("file_complete", fileIdHex);
            }).catch(reject);
        });
    },
    acceptFile(_fileId, _destPath) {
        // In PWA, auto-accept is handled by handleMessage — file chunks are
        // buffered automatically and the file is downloaded on completion.
        // In PWA, accept is implicit — chunks are buffered automatically
        // and the file is downloaded on completion.
        return Promise.resolve();
    },
    getTransfers() {
        return Promise.resolve([]);
    },
    generateQr(_linkId) {
        // QR generation is optional — could add a JS QR library later.
        return Promise.resolve("");
    },
};
export function onConnected(cb) {
    return Promise.resolve(on("connected", cb));
}
export function onDisconnected(cb) {
    return Promise.resolve(on("disconnected", cb));
}
export function onPeerConnected(cb) {
    return Promise.resolve(on("peer_connected", cb));
}
export function onMessage(cb) {
    return Promise.resolve(on("message", cb));
}
export function onFileOffered(cb) {
    return Promise.resolve(on("file_offered", cb));
}
export function onFileProgress(cb) {
    return Promise.resolve(on("file_progress", cb));
}
export function onFileComplete(cb) {
    return Promise.resolve(on("file_complete", cb));
}
export function onError(cb) {
    return Promise.resolve(on("error", cb));
}
/** Set the active peer for file sending. Called from stores. */
export function setActivePeerForTransfer(peerId) {
    globalThis.__p2pActivePeer = peerId;
}
