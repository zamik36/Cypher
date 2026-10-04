/** Contract between the shared UI and a runtime (Tauri desktop or browser). */

export type MessageStatus = "pending" | "sent" | "queued" | "delivered" | "read" | "failed";

export interface LinkInfo {
  link_id: string;
}

export interface UiFile {
  file_id: string;
  name: string;
  size: number;
  mime: string;
  kind: "file" | "voice" | "video_note";
  duration_ms: number | null;
  waveform?: number[] | null;
  poster?: number[] | null;
}

export interface MediaSent {
  msg_id: string;
  file_id: string;
  duration_ms: number;
  waveform?: number[];
}

/** A finished round video from the shared recorder. */
export interface VideoNote {
  blob: Blob;
  mime: string;
  durationMs: number;
  poster: Uint8Array;
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
  msg_id?: string;
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
export interface FileOffer {
  from: string;
  file_id: string;
  name: string;
  size: number;
  mime: string;
}

export type Unsubscribe = () => void;

/** Core event channels and their payloads (see `cypher_core::ui::event`). */
export interface Events {
  connected: null;
  disconnected: null;
  /** Another device signed in with this identity; this one stopped. */
  superseded: null;
  /** The server speaks another protocol version; this client stopped. */
  update_required: null;
  peer_connected: string;
  message: UiMessage;
  message_status: { msg_id: string; status: MessageStatus };
  file_offered: FileOffer;
  file_progress: { file_id: string; progress: number };
  file_complete: string;
  file_failed: { file_id: string; reason: string };
  error: string;
  anonymity_level: AnonymityLevelPayload;
}

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
  /** The 60 digits both sides of a conversation compare to rule out a man in the middle. */
  safetyNumber(peerId: string): Promise<string>;
  /** Takes the session back after another device took it over. */
  reconnect(): Promise<void>;
  /** Starts a voice note; `onLevel` receives 0..1 loudness for the meter. */
  startVoice(onLevel: (level: number) => void): Promise<void>;
  /** Stops and sends the voice note; `null` when it was too short. */
  stopVoice(peerId: string): Promise<MediaSent | null>;
  cancelVoice(): Promise<void>;
  sendVideoNote(peerId: string, note: VideoNote): Promise<MediaSent>;
  /** URL an `<audio>`/`<video>` element can play a stored note from. */
  mediaUrl(fileId: string): Promise<string>;
  getConversations(): Promise<ConversationEntry[]>;
  getHistory(peerId: string, limit: number, before?: number): Promise<UiMessage[]>;
  clearChatHistory(): Promise<void>;
  /** Subscribes to a core event channel. */
  on<K extends keyof Events>(channel: K, cb: (payload: Events[K]) => void): Promise<Unsubscribe>;
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
    return typeof value === "function" ? (value as () => unknown).bind(current) : value;
  },
});

export const onConnected = (cb: () => void) => api.on("connected", () => cb());
export const onDisconnected = (cb: () => void) => api.on("disconnected", () => cb());
export const onSuperseded = (cb: () => void) => api.on("superseded", () => cb());
export const onUpdateRequired = (cb: () => void) => api.on("update_required", () => cb());
export const onPeerConnected = (cb: (peerId: string) => void) => api.on("peer_connected", cb);
export const onMessage = (cb: (msg: UiMessage) => void) => api.on("message", cb);
export const onMessageStatus = (cb: (p: Events["message_status"]) => void) => api.on("message_status", cb);
export const onFileOffered = (cb: (info: FileOffer) => void) => api.on("file_offered", cb);
export const onFileProgress = (cb: (info: Events["file_progress"]) => void) => api.on("file_progress", cb);
export const onFileComplete = (cb: (fileId: string) => void) => api.on("file_complete", cb);
export const onFileFailed = (cb: (info: Events["file_failed"]) => void) => api.on("file_failed", cb);
export const onError = (cb: (msg: string) => void) => api.on("error", cb);
export const onAnonymityLevel = (cb: (payload: AnonymityLevelPayload) => void) => api.on("anonymity_level", cb);
