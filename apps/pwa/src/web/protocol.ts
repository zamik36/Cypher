/** Messages between the UI thread and the core worker. */

export interface Methods {
  hasIdentity(): boolean;
  createIdentity(nickname: string, passphrase: string): string;
  unlockIdentity(passphrase: string): [string, string];
  importMnemonic(mnemonic: string, nickname: string, passphrase: string): string;
  exportMnemonic(passphrase: string): string;
  /** Stops the client and drops the identity from memory. */
  lock(): void;
  /** Deletes the identity and everything kept with it. */
  eraseDevice(): void;
  connect(gatewayUrl: string, relayUrl: string, anonymous: boolean): string;
  setAnonymity(anonymous: boolean): void;
  createLink(): string;
  joinLink(link: string): string;
  command(cmd: Record<string, unknown>): { msgId?: string; fileId?: string };
  sendFiles(peer: string, files: File[]): { msg_id: string; file_id: string; file_name: string; total_size: number }[];
  sendMedia(
    peer: string,
    blob: Blob,
    mime: string,
    kind: "voice" | "video_note",
    durationMs: number,
    extra: { frames?: Float32Array; poster?: Uint8Array },
  ): { msg_id: string; file_id: string; duration_ms: number; waveform?: number[] };
  mediaBlob(fileId: string): Blob;
  /** Whether a received file is still kept (OPFS `downloads`). */
  fileSaved(fileId: string): boolean;
  savedBlob(fileId: string): Blob;
  qr(text: string): string;
  safetyNumber(peer: string): string;
  /** Takes the session back after another device took it over. */
  reconnect(): void;
  conversations(): {
    peer_id: string;
    alias: string | null;
    name: string | null;
    request: boolean;
    blocked: boolean;
    last_message_at: number;
    last: unknown;
    unread: number;
  }[];
  /** Deletes one message from this device; `timestamp` within a minute of its own. */
  deleteMessage(peer: string, msgId: string, timestamp: number): void;
  /** Ends the session with `peer` and deletes the conversation. */
  forgetPeer(peer: string): void;
  history(peer: string, limit: number, before?: number): unknown[];
  clearHistory(): void;
  /** Starts waiting to be linked as `name` through `gatewayUrl`: the offer to show. */
  startLink(gatewayUrl: string, name: string): string;
  /** Waits for the hand-over and keeps it under `passphrase`: peer id and nickname. */
  finishLink(passphrase: string): [string, string];
  cancelLink(): void;
  /** Links the new device showing `offer`; resolves once it is listed. */
  linkDevice(offer: string): void;
}

export type Method = keyof Methods;

export interface Request<M extends Method = Method> {
  id: number;
  method: M;
  args: Parameters<Methods[M]>;
}

export type WorkerMessage =
  | { id: number; ok: true; result: unknown }
  | { id: number; ok: false; error: string }
  | { event: string; payload: unknown }
  | { download: { fileId: string; name: string; blob: Blob } };
