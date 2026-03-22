import { createStore } from "solid-js/store";
/** Per-peer message storage: peerId → ChatMessage[] */
const [chatsByPeer, setChatsByPeer] = createStore({});
export function addMessage(peerId, msg) {
    setChatsByPeer((prev) => ({
        ...prev,
        [peerId]: [...(prev[peerId] || []), msg],
    }));
}
export function getMessages(peerId) {
    return chatsByPeer[peerId] || [];
}
export function clearMessages(peerId) {
    setChatsByPeer((prev) => ({ ...prev, [peerId]: [] }));
}
export function clearAllMessages() {
    setChatsByPeer({});
}
export { chatsByPeer };
