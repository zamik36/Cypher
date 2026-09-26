/** Contract between the shared UI and a runtime (Tauri desktop or browser). */

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

export interface AnonymityLevelPayload { level: number; label: string; description: string; }
export interface FileOffer { from: string; file_id: string; name: string; size: number; mime: string; }

export type Unsubscribe = () => void;

export interface Notifications {
  supported(): boolean;
  permission(): Promise<NotificationPermission>;
  request(): Promise<NotificationPermission>;
  send(title: string, body: string): Promise<void>;
}

export interface Platform {
  readonly kind: "desktop" | "web";
  hasIdentity(): Promise<boolean>;
  createIdentity(nickname: string, passphrase: string): Promise<string>;
  unlockIdentity(passphrase: string): Promise<[string, string]>;
  importMnemonic(mnemonic: string, nickname: string, passphrase: string): Promise<string>;
  exportMnemonic(passphrase: string): Promise<string>;
  connectToGateway(addr: string, anonymous: boolean, bridges: string[]): Promise<string>;
  applyAnonymousSettings(anonymous: boolean, bridges: string[]): Promise<void>;
  createLink(): Promise<LinkInfo>;
  joinLink(linkId: string): Promise<string>;
  sendMessage(peerId: string, text: string): Promise<string>;
  markRead(peerId: string, msgIds: string[]): Promise<void>;
  /** Lets the user pick files and offers them to `peerId`. */
  pickAndSend(peerId: string): Promise<TransferInfo[]>;
  acceptFile(fileId: string): Promise<void>;
  cancelTransfer(fileId: string): Promise<void>;
  generateQr(linkId: string): Promise<string>;
  getConversations(): Promise<ConversationEntry[]>;
  getHistory(peerId: string, limit: number, before?: number): Promise<UiMessage[]>;
  clearChatHistory(): Promise<void>;
  /** Subscribes to a core event channel (see `cypher_core::ui::event`). */
  on<T>(channel: string, cb: (payload: T) => void): Promise<Unsubscribe>;
  readonly notifications: Notifications;
}

let current: Platform | null = null;

export function registerPlatform(platform: Platform): void {
  current = platform;
}

/** The registered platform; methods resolve lazily so modules can import early. */
export const api: Platform = new Proxy({} as Platform, {
  get(_target, key) {
    if (!current) throw new Error("platform not registered");
    const value = (current as unknown as Record<PropertyKey, unknown>)[key];
    return typeof value === "function" ? value.bind(current) : value;
  },
});

export const onConnected = (cb: () => void) => api.on<void>("connected", () => cb());
export const onDisconnected = (cb: () => void) => api.on<void>("disconnected", () => cb());
export const onPeerConnected = (cb: (peerId: string) => void) => api.on<string>("peer_connected", cb);
export const onMessage = (cb: (msg: UiMessage) => void) => api.on<UiMessage>("message", cb);
export const onMessageStatus = (cb: (p: { msg_id: string; status: MessageStatus }) => void) =>
  api.on<{ msg_id: string; status: MessageStatus }>("message_status", cb);
export const onFileOffered = (cb: (info: FileOffer) => void) => api.on<FileOffer>("file_offered", cb);
export const onFileProgress = (cb: (info: { file_id: string; progress: number }) => void) =>
  api.on<{ file_id: string; progress: number }>("file_progress", cb);
export const onFileComplete = (cb: (fileId: string) => void) => api.on<string>("file_complete", cb);
export const onFileFailed = (cb: (info: { file_id: string; reason: string }) => void) =>
  api.on<{ file_id: string; reason: string }>("file_failed", cb);
export const onError = (cb: (msg: string) => void) => api.on<string>("error", cb);
export const onAnonymityLevel = (cb: (payload: AnonymityLevelPayload) => void) =>
  api.on<AnonymityLevelPayload>("anonymity_level", cb);
