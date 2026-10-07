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
  /** The message this one answers. */
  reply_to?: string | null;
}

export interface UiMessage {
  msg_id: string;
  from: string;
  outgoing: boolean;
  text: string;
  timestamp: number;
  status: MessageStatus;
  file: UiFile | null;
  reply_to: string | null;
}

export interface TransferInfo {
  msg_id?: string;
  file_id: string;
  file_name: string;
  total_size: number;
  progress: number;
  direction: "send" | "receive";
  status: "offered" | "active" | "complete" | "error";
  /** Why it failed, in the user's language. */
  error?: string;
}

/** A conversation as the chat list shows it, most recent first. */
export interface ConversationEntry {
  peer_id: string;
  /** The name the user gave this contact on this device. */
  alias: string | null;
  /** The name the contact goes by, as they last sent it. */
  name: string | null;
  /** Wrote without one of our invites; not accepted yet. */
  request: boolean;
  blocked: boolean;
  last_message_at: number;
  last: UiMessage | null;
  /** Incoming messages not read yet (counted over the latest hundred). */
  unread: number;
}

/** 1 while inbox traffic goes through the onion relay, 0 otherwise. */
export interface AnonymityLevelPayload {
  level: number;
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
  /** Files are being dragged over the window (native drops only). */
  files_dragging: boolean;
  /** Files were dropped on the window; `sendDropped` offers them. */
  files_dropped: { id: number; names: string[] };
  /** Someone wrote without one of our invites. */
  contact_request: string;
  /** A contact told us the name they go by (or that they have none). */
  peer_profile: { peer: string; name: string | null };
  message: UiMessage;
  message_status: { msg_id: string; status: MessageStatus };
  file_offered: FileOffer;
  file_progress: { file_id: string; progress: number };
  file_complete: string;
  file_failed: { file_id: string; reason: string };
  error: string;
  anonymity_level: AnonymityLevelPayload;
  /** The server's push key (hex), to subscribe this device with. */
  push_key: string;
  /** Whether the server will wake this device while the app is closed. */
  push_state: "registered" | "unavailable";
}

export interface Notifications {
  supported(): boolean;
  permission(): Promise<NotificationPermission>;
  request(): Promise<NotificationPermission>;
  send(title: string, body: string): Promise<void>;
}

/** What the runtime can do; the UI shows only what works here. */
export interface Capabilities {
  /** Inbox traffic can go through Tor (the native client, with vanilla bridges). */
  tor: boolean;
  /** The server address is the user's to choose (desktop). */
  gatewayConfig: boolean;
  /** A received file can be shown in its folder (desktop). */
  revealFile: boolean;
  /** A note plays while it is still downloading (ranged reads of what came). */
  streamMedia: boolean;
}

export interface Platform {
  readonly kind: "desktop" | "web";
  readonly capabilities: Capabilities;
  hasIdentity(): Promise<boolean>;
  createIdentity(nickname: string, passphrase: string): Promise<string>;
  unlockIdentity(passphrase: string): Promise<[string, string]>;
  importMnemonic(mnemonic: string, nickname: string, passphrase: string): Promise<string>;
  exportMnemonic(passphrase: string): Promise<string>;
  /** The desktop's tray and window behaviour; absent elsewhere. */
  shell?: {
    setTray(open: string, quit: string, tooltip: string): Promise<void>;
    setCloseToTray(enabled: boolean): Promise<void>;
  };
  /**
   * Wake-ups while the app is closed (Web Push); absent where the platform
   * has none. The server learns only "this inbox has news".
   */
  push?: {
    /** Asks the server for its key, which arrives on `push_key`. */
    enable(): Promise<void>;
    /** Subscribes this device with that key and registers it; false if refused. */
    subscribe(keyHex: string): Promise<boolean>;
    /** Unsubscribes and has the server forget the subscription. */
    disable(): Promise<void>;
  };
  /** Stops the client and drops the identity from memory until unlocked. */
  lock(): Promise<void>;
  /** Deletes the profile and everything kept with it from this device. */
  eraseDevice(): Promise<void>;
  connectToGateway(addr: string, anonymous: boolean, bridges: string[]): Promise<string>;
  applyAnonymousSettings(anonymous: boolean, bridges: string[]): Promise<void>;
  createLink(): Promise<LinkInfo>;
  joinLink(linkId: string): Promise<string>;
  sendMessage(peerId: string, text: string, replyTo?: string): Promise<string>;
  /** Deletes a message from this device only; `timestamp` as the UI shows it. */
  deleteMessage(peerId: string, msgId: string, timestamp: number): Promise<void>;
  markRead(peerId: string, msgIds: string[]): Promise<void>;
  /** Offers files the UI holds (dropped in a browser, pasted) to `peerId`. */
  sendFiles(peerId: string, files: File[]): Promise<TransferInfo[]>;
  /** Offers the files of a native drop (`files_dropped`) to `peerId`. */
  sendDropped?: (peerId: string, dropId: number) => Promise<TransferInfo[]>;
  /** Lets the user pick files and offers them to `peerId`. */
  pickAndSend(peerId: string): Promise<TransferInfo[]>;
  acceptFile(fileId: string): Promise<void>;
  cancelTransfer(fileId: string): Promise<void>;
  /** Whether a finished file can still be opened from this device. */
  fileSaved(fileId: string): Promise<boolean>;
  /** Opens a finished file with another app (or, on the web, saves it again). */
  openFile(file: UiFile): Promise<void>;
  /** Shows a finished file in its folder; only with `capabilities.revealFile`. */
  revealFile(fileId: string): Promise<void>;
  generateQr(linkId: string): Promise<string>;
  /**
   * Reads a QR code with the system scanner, which runs behind the page
   * (Android). Without it the UI uses the camera itself.
   */
  scanQr?: () => Promise<string>;
  cancelScan?: () => Promise<void>;
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
  /** A URL showing a kept picture sent or received as a file. */
  imageUrl(fileId: string): Promise<string>;
  getConversations(): Promise<ConversationEntry[]>;
  /** Takes someone who wrote without an invite as a contact. */
  acceptContact(peerId: string): Promise<void>;
  setBlocked(peerId: string, blocked: boolean): Promise<void>;
  /** Names a contact on this device; `null` or blank removes the name. */
  renamePeer(peerId: string, alias: string | null): Promise<void>;
  /** Ends the session with a contact and deletes the conversation. */
  deleteConversation(peerId: string): Promise<void>;
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
export const onContactRequest = (cb: (peerId: string) => void) => api.on("contact_request", cb);
export const onPeerProfile = (cb: (p: Events["peer_profile"]) => void) => api.on("peer_profile", cb);
export const onMessage = (cb: (msg: UiMessage) => void) => api.on("message", cb);
export const onMessageStatus = (cb: (p: Events["message_status"]) => void) => api.on("message_status", cb);
export const onFileOffered = (cb: (info: FileOffer) => void) => api.on("file_offered", cb);
export const onFileProgress = (cb: (info: Events["file_progress"]) => void) => api.on("file_progress", cb);
export const onFileComplete = (cb: (fileId: string) => void) => api.on("file_complete", cb);
export const onFileFailed = (cb: (info: Events["file_failed"]) => void) => api.on("file_failed", cb);
export const onError = (cb: (msg: string) => void) => api.on("error", cb);
export const onAnonymityLevel = (cb: (payload: AnonymityLevelPayload) => void) => api.on("anonymity_level", cb);
