import { createStore, produce, reconcile } from "solid-js/store";
import type { ChatMessage, MessageStatus } from "../platform";

/** Max messages kept in memory per peer; older ones are reloaded on demand. */
const MAX_IN_MEMORY = 1500;

const [chatsByPeer, setChatsByPeer] = createStore<Record<string, ChatMessage[]>>({});
const peerOfMessage = new Map<string, string>();
/** Peers whose stored history is merged in; the others load it when first shown. */
const historyMerged = new Set<string>();

export function addMessage(peerId: string, msg: ChatMessage) {
  if (msg.msg_id) {
    if (peerOfMessage.has(msg.msg_id)) return;
    peerOfMessage.set(msg.msg_id, peerId);
  }
  const list = chatsByPeer[peerId];
  if (!list) {
    setChatsByPeer(peerId, [msg]);
    return;
  }
  setChatsByPeer(peerId, list.length, msg);
  if (list.length > MAX_IN_MEMORY) {
    setChatsByPeer(
      peerId,
      produce((list) => {
        for (const evicted of list.splice(0, list.length - MAX_IN_MEMORY)) {
          if (evicted.msg_id) peerOfMessage.delete(evicted.msg_id);
        }
      }),
    );
  }
}

export function setMessageStatus(msgId: string, status: MessageStatus) {
  const peerId = peerOfMessage.get(msgId);
  if (!peerId) return;
  setChatsByPeer(peerId, (m) => m.msg_id === msgId, "status", status);
}

export function peerOf(msgId: string): string | undefined {
  return peerOfMessage.get(msgId);
}

export function getMessages(peerId: string): ChatMessage[] {
  return chatsByPeer[peerId] ?? [];
}

export function setMessages(peerId: string, msgs: ChatMessage[]) {
  for (const m of msgs) if (m.msg_id) peerOfMessage.set(m.msg_id, peerId);
  setChatsByPeer(peerId, msgs);
}

export function historyLoaded(peerId: string): boolean {
  return historyMerged.has(peerId);
}

/**
 * Puts stored history (oldest first) before what arrived live since the app
 * started. A message in both keeps its place in history and its live copy,
 * whose status may be newer.
 */
export function mergeHistory(peerId: string, history: ChatMessage[]) {
  historyMerged.add(peerId);
  const live = getMessages(peerId);
  const fresh = new Map<string, ChatMessage>();
  for (const m of live) if (m.msg_id) fresh.set(m.msg_id, m);
  const merged = history.map((m) => (m.msg_id ? fresh.get(m.msg_id) : undefined) ?? m);
  const stored = new Set(history.flatMap((m) => (m.msg_id ? [m.msg_id] : [])));
  setMessages(peerId, [...merged, ...live.filter((m) => !m.msg_id || !stored.has(m.msg_id))]);
}

export function clearAllMessages() {
  historyMerged.clear();
  peerOfMessage.clear();
  setChatsByPeer(reconcile({}));
}

export { chatsByPeer };
