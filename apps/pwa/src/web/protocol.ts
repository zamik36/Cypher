/** Messages between the UI thread and the core worker. */

export interface Methods {
  hasIdentity(): boolean;
  createIdentity(nickname: string, passphrase: string): string;
  unlockIdentity(passphrase: string): [string, string];
  importMnemonic(mnemonic: string, nickname: string, passphrase: string): string;
  exportMnemonic(passphrase: string): string;
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
    last_message_at: number;
    last: unknown;
    unread: number;
  }[];
  /** Ends the session with `peer` and deletes the conversation. */
  forgetPeer(peer: string): void;
  history(peer: string, limit: number, before?: number): unknown[];
  clearHistory(): void;
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
