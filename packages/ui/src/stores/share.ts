import { createSignal } from "solid-js";
import type { TransferInfo } from "../platform";
import { readyMark, retryWhenReady } from "./clientReady";

/** What another app shared with this one (the PWA's share target). */
export interface Shared {
  files: File[];
  text: string;
}

let ready = false;
let waiting: Shared | null = null;
/** Picked for a chat, until that chat opens and sends it. */
let chosen: { peerId: string; share: Shared } | null = null;
const [pending, setPending] = createSignal<Shared | null>(null);

/** A share waiting for the user to choose a chat; the chat list asks. */
export const pendingShare = pending;

/** Keeps a share until the app is unlocked, then asks for a chat. */
export function receiveShare(share: Shared): void {
  if (share.files.length === 0 && !share.text.trim()) return;
  if (ready) setPending(share);
  else waiting = share;
}

/** Whether the app can ask now (unlocked); locking drops what was shared. */
export function setSharesReady(on: boolean): void {
  ready = on;
  if (on && waiting) setPending(waiting);
  if (!on) {
    setPending(null);
    chosen = null;
  }
  waiting = null;
}

export function cancelShare(): void {
  setPending(null);
}

/** Sends the pending share to `peerId` once its chat opens. */
export function shareWith(peerId: string): void {
  const share = pending();
  if (!share) return;
  setPending(null);
  chosen = { peerId, share };
}

/** The share chosen for `peerId`, handed over once. */
export function takeShare(peerId: string): Shared | null {
  if (chosen?.peerId !== peerId) return null;
  const { share } = chosen;
  chosen = null;
  return share;
}

/**
 * Sends shared `files` one at a time, each result to `sent`. A file that
 * could not go because the client was still starting waits, with the files
 * after it, until the client is up, so no file goes twice; any other
 * failure goes to `failed` and the rest still go.
 */
export function sendEach(
  files: File[],
  send: (file: File) => Promise<TransferInfo[]>,
  sent: (cards: TransferInfo[]) => void,
  failed: (error: unknown) => void,
): void {
  const [file, ...rest] = files;
  if (!file) return;
  const mark = readyMark();
  const next = () => sendEach(rest, send, sent, failed);
  send(file).then(
    (cards) => {
      sent(cards);
      next();
    },
    (error: unknown) => {
      if (retryWhenReady(error, mark, () => sendEach(files, send, sent, failed))) return;
      failed(error);
      next();
    },
  );
}
