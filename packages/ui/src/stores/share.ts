import { createSignal } from "solid-js";

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
