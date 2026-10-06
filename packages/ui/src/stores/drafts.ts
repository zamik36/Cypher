import { createStore, reconcile } from "solid-js/store";

/** What the user was writing in a chat, and to which message. */
export interface Draft {
  text: string;
  replyTo: string | null;
}

const EMPTY: Draft = { text: "", replyTo: null };

/**
 * Drafts per chat, kept in memory only: they survive switching chats, not
 * locking or closing the app, so no plaintext lands on disk.
 */
const [drafts, setDrafts] = createStore<Record<string, Draft>>({});

export function draftOf(peerId: string): Draft {
  return drafts[peerId] ?? EMPTY;
}

export function setDraftText(peerId: string, text: string): void {
  setDrafts(peerId, { ...draftOf(peerId), text });
}

export function setReplyTo(peerId: string, replyTo: string | null): void {
  setDrafts(peerId, { ...draftOf(peerId), replyTo });
}

export function clearDraft(peerId: string): void {
  setDrafts(peerId, { ...EMPTY });
}

/** Forgets every draft (on lock). */
export function clearAllDrafts(): void {
  setDrafts(reconcile({}));
}
