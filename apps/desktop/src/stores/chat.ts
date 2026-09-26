import { createStore, produce } from "solid-js/store";
import type { ChatMessage, MessageStatus } from "../api/tauri";

/** Max messages kept in memory per peer; older ones are reloaded on demand. */
const MAX_IN_MEMORY = 1500;

const [chatsByPeer, setChatsByPeer] = createStore<Record<string, ChatMessage[]>>({});
const peerOfMessage = new Map<string, string>();

export function addMessage(peerId: string, msg: ChatMessage) {
  if (msg.msg_id) {
    if (peerOfMessage.has(msg.msg_id)) return;
    peerOfMessage.set(msg.msg_id, peerId);
  }
  if (!chatsByPeer[peerId]) setChatsByPeer(peerId, []);
  setChatsByPeer(peerId, chatsByPeer[peerId].length, msg);
  if (chatsByPeer[peerId].length > MAX_IN_MEMORY) {
    setChatsByPeer(peerId, produce((list) => {
      for (const evicted of list.splice(0, list.length - MAX_IN_MEMORY)) {
        if (evicted.msg_id) peerOfMessage.delete(evicted.msg_id);
      }
    }));
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
  return chatsByPeer[peerId] || [];
}

export function setMessages(peerId: string, msgs: ChatMessage[]) {
  for (const m of msgs) if (m.msg_id) peerOfMessage.set(m.msg_id, peerId);
  setChatsByPeer(peerId, msgs);
}

export function clearAllMessages() {
  peerOfMessage.clear();
  setChatsByPeer(produce((all) => {
    for (const key of Object.keys(all)) delete all[key];
  }));
}

export { chatsByPeer };
