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
  sendFiles(peer: string, files: File[]): { file_id: string; file_name: string; total_size: number }[];
  releaseDownload(fileId: string): void;
  qr(text: string): string;
  conversations(): { peer_id: string; display_name: null; last_message_at: number }[];
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
