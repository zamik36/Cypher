import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type MessageStatus = "pending" | "sent" | "queued" | "delivered" | "read" | "failed";

export interface LinkInfo { link_id: string; }

export interface UiFile {
  file_id: string;
  name: string;
  size: number;
  mime: string;
  kind: "file" | "voice" | "video_note";
  duration_ms: number | null;
}

export interface ChatMessage {
  msg_id?: string;
  from: string;
  text: string;
  timestamp: number;
  status?: MessageStatus;
  file?: UiFile | null;
}

export interface UiMessage {
  msg_id: string;
  from: string;
  outgoing: boolean;
  text: string;
  timestamp: number;
  status: MessageStatus;
  file: UiFile | null;
}

export interface TransferInfo {
  file_id: string;
  file_name: string;
  total_size: number;
  progress: number;
  direction: "send" | "receive";
  status: "offered" | "active" | "complete" | "error";
}

export interface ConversationEntry {
  peer_id: string;
  display_name: string | null;
  last_message_at: number;
}

export interface AnonymityLevelPayload {
  level: number;
  label: string;
  description: string;
}

export interface FileOffer { from: string; file_id: string; name: string; size: number; mime: string; }

type TauriListenerCleanup = () => void;

function isTauriRuntimeAvailable() {
  return typeof globalThis !== "undefined" && isTauri();
}

function invokeCommand<T>(command: string, args?: Record<string, unknown>, fallback?: () => T): Promise<T> {
  if (!isTauriRuntimeAvailable()) {
    return fallback
      ? Promise.resolve(fallback())
      : Promise.reject(new Error(`${command} is unavailable outside the Tauri runtime.`));
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
  connectToGateway: (addr: string, requireOnion: boolean) =>
    invokeCommand<string>("connect_to_gateway", { addr, requireOnion }),
  createLink: () => invokeCommand<LinkInfo>("create_link"),
  joinLink: (linkId: string) => invokeCommand<string>("join_link", { linkId }),
  sendMessage: (peerId: string, text: string) => invokeCommand<string>("send_message", { peerId, text }),
  markRead: (peerId: string, msgIds: string[]) => invokeCommand<void>("mark_read", { peerId, msgIds }),
  browseAndSend: (peerId: string) => invokeCommand<TransferInfo[]>("browse_and_send", { peerId }),
  acceptFile: (fileId: string) => invokeCommand<void>("accept_file", { fileId }),
  cancelTransfer: (fileId: string) => invokeCommand<void>("cancel_transfer", { fileId }),
  generateQr: (linkId: string) => invokeCommand<string>("generate_qr", { linkId }),
  hasIdentity: () => invokeCommand<boolean>("has_identity", undefined, () => false),
  createIdentity: (nickname: string, passphrase: string) => invokeCommand<string>("create_identity", { nickname, passphrase }),
  unlockIdentity: (passphrase: string) => invokeCommand<[string, string]>("unlock_identity", { passphrase }),
  exportMnemonic: (passphrase: string) => invokeCommand<string>("export_mnemonic", { passphrase }),
  importMnemonic: (mnemonic: string, nickname: string, passphrase: string) =>
    invokeCommand<string>("import_mnemonic", { mnemonic, nickname, passphrase }),
  getConversations: () => invokeCommand<ConversationEntry[]>("get_conversations"),
  getHistory: (peerId: string, limit: number, before?: number) =>
    invokeCommand<UiMessage[]>("get_history", { peerId, limit, before }),
  clearChatHistory: () => invokeCommand<void>("clear_chat_history"),
  applyAnonymousSettings: (requireOnion: boolean) =>
    invokeCommand<void>("apply_anonymous_settings", { requireOnion }),
};

export const onConnected = (cb: () => void) => listenToEvent<void>("cypher://connected", () => cb());
export const onDisconnected = (cb: () => void) => listenToEvent<void>("cypher://disconnected", () => cb());
export const onPeerConnected = (cb: (peerId: string) => void) => listenToEvent<string>("cypher://peer_connected", cb);
export const onMessage = (cb: (msg: UiMessage) => void) => listenToEvent<UiMessage>("cypher://message", cb);
export const onMessageStatus = (cb: (p: { msg_id: string; status: MessageStatus }) => void) =>
  listenToEvent<{ msg_id: string; status: MessageStatus }>("cypher://message_status", cb);
export const onFileOffered = (cb: (info: FileOffer) => void) => listenToEvent<FileOffer>("cypher://file_offered", cb);
export const onFileProgress = (cb: (info: { file_id: string; progress: number }) => void) =>
  listenToEvent<{ file_id: string; progress: number }>("cypher://file_progress", cb);
export const onFileComplete = (cb: (fileId: string) => void) => listenToEvent<string>("cypher://file_complete", cb);
export const onFileFailed = (cb: (info: { file_id: string; reason: string }) => void) =>
  listenToEvent<{ file_id: string; reason: string }>("cypher://file_failed", cb);
export const onError = (cb: (msg: string) => void) => listenToEvent<string>("cypher://error", cb);
export const onAnonymityLevel = (cb: (payload: AnonymityLevelPayload) => void) =>
  listenToEvent<AnonymityLevelPayload>("cypher://anonymity_level", cb);
