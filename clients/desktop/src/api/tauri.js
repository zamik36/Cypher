import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
export const api = {
    connectToGateway: (addr) => invoke("connect_to_gateway", { addr }),
    createLink: () => invoke("create_link"),
    joinLink: (linkId) => invoke("join_link", { linkId }),
    sendMessage: (peerId, text) => invoke("send_message", { peerId, text }),
    getMessages: () => invoke("get_messages"),
    sendFile: (path) => invoke("send_file", { path }),
    browseAndSend: () => invoke("browse_and_send"),
    acceptFile: (fileId, destPath) => invoke("accept_file", { fileId, destPath }),
    getTransfers: () => invoke("get_transfers"),
    generateQr: (linkId) => invoke("generate_qr", { linkId }),
};
export function onConnected(cb) {
    return listen("p2p://connected", (e) => cb(e.payload));
}
export function onDisconnected(cb) {
    return listen("p2p://disconnected", () => cb());
}
export function onPeerConnected(cb) {
    return listen("p2p://peer_connected", (e) => cb(e.payload));
}
export function onMessage(cb) {
    return listen("p2p://message", (e) => cb(e.payload));
}
export function onFileOffered(cb) {
    return listen("p2p://file_offered", (e) => cb(e.payload));
}
export function onFileProgress(cb) {
    return listen("p2p://file_progress", (e) => cb(e.payload));
}
export function onFileComplete(cb) {
    return listen("p2p://file_complete", (e) => cb(e.payload));
}
export function onError(cb) {
    return listen("p2p://error", (e) => cb(e.payload));
}
