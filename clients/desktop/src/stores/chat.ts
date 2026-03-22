import { createStore } from "solid-js/store";
import type { ChatMessage } from "../api/tauri";

/** Per-peer message storage: peerId → ChatMessage[] */
const [chatsByPeer, setChatsByPeer] = createStore<Record<string, ChatMessage[]>>({});

export function addMessage(peerId: string, msg: ChatMessage) {
  setChatsByPeer((prev) => ({
    ...prev,
    [peerId]: [...(prev[peerId] || []), msg],
  }));
}

export function getMessages(peerId: string): ChatMessage[] {
  return chatsByPeer[peerId] || [];
}

export function clearMessages(peerId: string) {
  setChatsByPeer((prev) => ({ ...prev, [peerId]: [] }));
}

export function clearAllMessages() {
  setChatsByPeer({});
}

export { chatsByPeer };
