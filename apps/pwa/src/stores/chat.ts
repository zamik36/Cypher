import { createStore } from "solid-js/store";
import type { ChatMessage } from "../api";

/** Max messages kept in memory per peer — older ones are evicted. */
const MAX_IN_MEMORY = 1500;

const [chatsByPeer, setChatsByPeer] = createStore<Record<string, ChatMessage[]>>({});

export function addMessage(peerId: string, msg: ChatMessage) {
  setChatsByPeer((prev) => {
    const existing = prev[peerId] || [];
    const updated = [...existing, msg];
    return { ...prev, [peerId]: updated.length > MAX_IN_MEMORY ? updated.slice(-MAX_IN_MEMORY) : updated };
  });
}

export function getMessages(peerId: string): ChatMessage[] {
  return chatsByPeer[peerId] || [];
}

export function clearMessages(peerId: string) {
  setChatsByPeer((prev) => ({ ...prev, [peerId]: [] }));
}

export function setMessages(peerId: string, msgs: ChatMessage[]) {
  setChatsByPeer((prev) => ({ ...prev, [peerId]: msgs }));
}

export function clearAllMessages() {
  setChatsByPeer({});
}

export { chatsByPeer };
