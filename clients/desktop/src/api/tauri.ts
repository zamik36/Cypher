import { invoke, isTauri } from "@tauri-apps/api/core";
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

export interface ConversationEntry {
  peer_id: string;
  display_name: string | null;
  created_at: number;
  last_message_at: number;
  inbox_id: string | null;
}
export interface HistoryMessage { direction: "sent" | "received"; text: string; timestamp: number; }
export interface AnonymityLevelPayload {
  level: number;
  label: string;
  description: string;
}

type TauriListenerCleanup = () => void;

function isTauriRuntimeAvailable() {
  if (typeof globalThis === "undefined") {
    return false;
  }

  return isTauri();
}

function tauriUnavailableError(method: string) {
  return new Error(`${method} is unavailable outside the Tauri runtime. Start the app with \`npm run tauri dev\`.`);
}

function invokeCommand<T>(command: string, args?: Record<string, unknown>, fallback?: () => T): Promise<T> {
  if (!isTauriRuntimeAvailable()) {
    if (fallback) {
      return Promise.resolve(fallback());
    }

    return Promise.reject(tauriUnavailableError(command));
  }

  return invoke<T>(command, args);
}

function listenToEvent<T>(event: string, cb: (payload: T) => void): Promise<TauriListenerCleanup> {
  if (!isTauriRuntimeAvailable()) {
    return Promise.resolve(() => {});
  }

  return listen<T>(event, (e) => cb(e.payload));
}

export const api = {
  connectToGateway: (addr: string) => invokeCommand<void>("connect_to_gateway", { addr }),
  createLink: () => invokeCommand<LinkInfo>("create_link"),
  joinLink: (linkId: string) => invokeCommand<string>("join_link", { linkId }),
  sendMessage: (peerId: string, text: string) => invokeCommand<void>("send_message", { peerId, text }),
  getMessages: () => invokeCommand<ChatMessage[]>("get_messages"),
  sendFile: (path: string) => invokeCommand<TransferInfo>("send_file", { path }),
  browseAndSend: () => invokeCommand<TransferInfo[]>("browse_and_send"),
  acceptFile: (fileId: string, destPath: string) => invokeCommand<void>("accept_file", { fileId, destPath }),
  getTransfers: () => invokeCommand<TransferInfo[]>("get_transfers"),
  generateQr: (linkId: string) => invokeCommand<string>("generate_qr", { linkId }),
  // Identity
  hasIdentity: () => invokeCommand<boolean>("has_identity", undefined, () => false),
  createIdentity: (nickname: string, passphrase: string) => invokeCommand<string>("create_identity", { nickname, passphrase }),
  unlockIdentity: (passphrase: string) => invokeCommand<[string, string]>("unlock_identity", { passphrase }),
  exportMnemonic: (passphrase: string) => invokeCommand<string>("export_mnemonic", { passphrase }),
  importMnemonic: (mnemonic: string, nickname: string, passphrase: string) => invokeCommand<string>("import_mnemonic", { mnemonic, nickname, passphrase }),
  // Chat history
  getConversations: () => invokeCommand<ConversationEntry[]>("get_conversations"),
  getConversation: (peerId: string) => invokeCommand<ConversationEntry | null>("get_conversation", { peerId }),
  getHistory: (peerId: string, limit: number) => invokeCommand<HistoryMessage[]>("get_history", { peerId, limit }),
  clearChatHistory: () => invokeCommand<void>("clear_chat_history"),
  applyAnonymousSettings: (enabled: boolean, bridgeLines: string[]) =>
    invokeCommand<void>("apply_anonymous_settings", { enabled, bridgeLines }),
};


export function onConnected(cb: (peerId: string) => void) {
  return listenToEvent<string>("cypher://connected", cb);
}

export function onDisconnected(cb: () => void) {
  return listenToEvent<void>("cypher://disconnected", () => cb());
}

export function onPeerConnected(cb: (peerId: string) => void) {
  return listenToEvent<string>("cypher://peer_connected", cb);
}

export function onMessage(cb: (msg: ChatMessage) => void) {
  return listenToEvent<ChatMessage>("cypher://message", cb);
}

export function onFileOffered(cb: (info: { from: string; file_id: string; name: string; size: number }) => void) {
  return listenToEvent<{ from: string; file_id: string; name: string; size: number }>("cypher://file_offered", cb);
}

export function onFileProgress(cb: (info: { file_id: string; progress: number }) => void) {
  return listenToEvent<{ file_id: string; progress: number }>("cypher://file_progress", cb);
}

export function onFileComplete(cb: (fileId: string) => void) {
  return listenToEvent<string>("cypher://file_complete", cb);
}

export function onError(cb: (msg: string) => void) {
  return listenToEvent<string>("cypher://error", cb);
}

export function onAnonymityLevel(cb: (payload: AnonymityLevelPayload) => void) {
  return listenToEvent<AnonymityLevelPayload>("cypher://anonymity_level", cb);
}
