import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface LinkInfo { link_id: string; }
export interface ChatMessage { from: string; text: string; timestamp: number; }
export interface TransferInfo {
  file_id: string;
  file_name: string;
  total_size: number;
  progress: number;
  direction: string;
  status: "active" | "complete" | "error";
}

export interface ConversationEntry { peer_id: string; display_name: string | null; last_ts: number; }
export interface HistoryMessage { direction: "sent" | "received"; text: string; timestamp: number; }

export const api = {
  connectToGateway: (addr: string) => invoke<void>("connect_to_gateway", { addr }),
  createLink: () => invoke<LinkInfo>("create_link"),
  joinLink: (linkId: string) => invoke<string>("join_link", { linkId }),
  sendMessage: (peerId: string, text: string) => invoke<void>("send_message", { peerId, text }),
  getMessages: () => invoke<ChatMessage[]>("get_messages"),
  sendFile: (path: string) => invoke<TransferInfo>("send_file", { path }),
  browseAndSend: () => invoke<TransferInfo[]>("browse_and_send"),
  acceptFile: (fileId: string, destPath: string) => invoke<void>("accept_file", { fileId, destPath }),
  getTransfers: () => invoke<TransferInfo[]>("get_transfers"),
  generateQr: (linkId: string) => invoke<string>("generate_qr", { linkId }),
  // Identity
  hasIdentity: () => invoke<boolean>("has_identity"),
  createIdentity: (nickname: string, passphrase: string) => invoke<string>("create_identity", { nickname, passphrase }),
  unlockIdentity: (passphrase: string) => invoke<[string, string]>("unlock_identity", { passphrase }),
  exportMnemonic: (passphrase: string) => invoke<string>("export_mnemonic", { passphrase }),
  importMnemonic: (mnemonic: string, nickname: string, passphrase: string) => invoke<string>("import_mnemonic", { mnemonic, nickname, passphrase }),
  // Chat history
  getConversations: () => invoke<ConversationEntry[]>("get_conversations"),
  getHistory: (peerId: string, limit: number) => invoke<HistoryMessage[]>("get_history", { peerId, limit }),
  clearChatHistory: () => invoke<void>("clear_chat_history"),
};


export function onConnected(cb: (peerId: string) => void) {
  return listen<string>("cypher://connected", (e) => cb(e.payload));
}

export function onDisconnected(cb: () => void) {
  return listen<void>("cypher://disconnected", () => cb());
}

export function onPeerConnected(cb: (peerId: string) => void) {
  return listen<string>("cypher://peer_connected", (e) => cb(e.payload));
}

export function onMessage(cb: (msg: ChatMessage) => void) {
  return listen<ChatMessage>("cypher://message", (e) => cb(e.payload));
}

export function onFileOffered(cb: (info: { from: string; file_id: string; name: string; size: number }) => void) {
  return listen<{ from: string; file_id: string; name: string; size: number }>(
    "cypher://file_offered", (e) => cb(e.payload)
  );
}

export function onFileProgress(cb: (info: { file_id: string; progress: number }) => void) {
  return listen<{ file_id: string; progress: number }>(
    "cypher://file_progress", (e) => cb(e.payload)
  );
}

export function onFileComplete(cb: (fileId: string) => void) {
  return listen<string>("cypher://file_complete", (e) => cb(e.payload));
}

export function onError(cb: (msg: string) => void) {
  return listen<string>("cypher://error", (e) => cb(e.payload));
}
