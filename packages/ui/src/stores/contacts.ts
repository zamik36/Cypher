import { createStore, produce, reconcile } from "solid-js/store";
import type { ChatMessage, ConversationEntry, MessageStatus } from "../platform";
import { t } from "../i18n";
import { toChatMessage } from "../utils/messages";

/** Everything the chat list needs to know about one contact. */
export interface Contact {
  peerId: string;
  /** The name the user gave them on this device. */
  alias: string | null;
  /** The name they go by, as they last sent it. */
  name: string | null;
  /** Wrote without one of our invites; not accepted yet. */
  request: boolean;
  blocked: boolean;
  last: ChatMessage | null;
  /** Time of the last message, for ordering; 0 before any. */
  lastAt: number;
  unread: number;
  online: boolean;
}

const [contacts, setContacts] = createStore<Record<string, Contact>>({});

function blank(peerId: string): Contact {
  return {
    peerId,
    alias: null,
    name: null,
    request: false,
    blocked: false,
    last: null,
    lastAt: 0,
    unread: 0,
    online: false,
  };
}

/** Replaces the list with the conversations the client reported. */
export function loadConversations(list: readonly ConversationEntry[]): void {
  const next: Record<string, Contact> = {};
  for (const c of list) {
    next[c.peer_id] = {
      peerId: c.peer_id,
      alias: c.alias,
      name: c.name,
      request: c.request,
      blocked: c.blocked,
      last: c.last ? toChatMessage(c.peer_id, c.last) : null,
      lastAt: c.last_message_at,
      unread: c.unread,
      online: contacts[c.peer_id]?.online ?? false,
    };
  }
  setContacts(reconcile(next));
}

/** Makes sure `peerId` is listed (a new contact, before any message). */
export function ensureContact(peerId: string, online = false): void {
  if (contacts[peerId]) {
    if (online) setContacts(peerId, "online", true);
    return;
  }
  setContacts(peerId, { ...blank(peerId), online, lastAt: Date.now() });
}

/**
 * Records a message in its conversation's summary. Incoming messages count
 * as unread unless the user is looking at that conversation.
 */
export function noteMessage(peerId: string, message: ChatMessage, viewing: boolean): void {
  ensureContact(peerId);
  setContacts(
    peerId,
    produce((c) => {
      if (message.timestamp >= c.lastAt || !c.last) {
        c.last = message;
        c.lastAt = message.timestamp;
      }
      const incoming = message.from === peerId;
      if (incoming && !viewing) c.unread += 1;
    }),
  );
}

/** Keeps the list's tick in step when the last message's status changes. */
export function setLastStatus(peerId: string, msgId: string, status: MessageStatus): void {
  if (contacts[peerId]?.last?.msg_id === msgId) setContacts(peerId, "last", "status", status);
}

/** Puts `message` (or nothing) as the list's preview, e.g. after a deletion. */
export function setLastMessage(peerId: string, message: ChatMessage | null): void {
  if (!contacts[peerId]) return;
  setContacts(peerId, "last", message);
  if (message) setContacts(peerId, "lastAt", message.timestamp);
}

export function markConversationRead(peerId: string): void {
  if (contacts[peerId]?.unread) setContacts(peerId, "unread", 0);
}

export function setContactOnline(peerId: string, online: boolean): void {
  if (contacts[peerId] && contacts[peerId].online !== online) setContacts(peerId, "online", online);
}

export function setAllOffline(): void {
  for (const id of Object.keys(contacts)) setContactOnline(id, false);
}

export function setAlias(peerId: string, alias: string | null): void {
  ensureContact(peerId);
  const cleaned = alias?.trim() ?? "";
  setContacts(peerId, "alias", cleaned === "" ? null : cleaned);
}

export function removeContact(peerId: string): void {
  setContacts(produce((all) => Reflect.deleteProperty(all, peerId)));
}

/** Records the name a contact goes by. */
export function setContactName(peerId: string, name: string | null): void {
  ensureContact(peerId);
  setContacts(peerId, "name", name?.trim() || null);
}

/** The name to show: the user's own for the contact, else theirs. */
const nameOf = (peerId: string) => contacts[peerId]?.alias ?? contacts[peerId]?.name ?? null;

/** What a contact's avatar spells: their name, or their id until they have one. */
export function avatarName(peerId: string): string {
  return nameOf(peerId) ?? peerId;
}

/** How a contact is called everywhere in the UI. */
export function displayName(peerId: string): string {
  return nameOf(peerId) ?? t().contact_fallback(peerId.slice(0, 6));
}

export function setContactRequest(peerId: string, request: boolean): void {
  ensureContact(peerId);
  setContacts(peerId, "request", request);
}

export function setContactBlocked(peerId: string, blocked: boolean): void {
  ensureContact(peerId);
  setContacts(peerId, "blocked", blocked);
}

const newestFirst = (a: Contact, b: Contact) => b.lastAt - a.lastAt;

/** Chats, most recent first, optionally only those matching `query` by name. */
export function sortedContacts(query = ""): Contact[] {
  const needle = query.trim().toLowerCase();
  return Object.values(contacts)
    .filter((c) => !c.request && !c.blocked)
    .filter((c) => needle === "" || displayName(c.peerId).toLowerCase().includes(needle))
    .sort(newestFirst);
}

/** People who wrote without an invite, waiting for an answer. */
export function requestContacts(): Contact[] {
  return Object.values(contacts)
    .filter((c) => c.request && !c.blocked)
    .sort(newestFirst);
}

export function blockedContacts(): Contact[] {
  return Object.values(contacts)
    .filter((c) => c.blocked)
    .map((c) => ({ c, name: displayName(c.peerId) }))
    .sort((a, b) => a.name.localeCompare(b.name))
    .map(({ c }) => c);
}

/** Unread in chats; requests and blocked contacts do not count. */
export function totalUnread(): number {
  return sortedContacts().reduce((sum, c) => sum + c.unread, 0);
}

export { contacts };
