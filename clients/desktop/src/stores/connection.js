import { createStore } from "solid-js/store";
const [connection, setConnection] = createStore({
    connected: false,
    peerId: null,
    gatewayAddr: "127.0.0.1:9100",
    status: "disconnected",
    gatewayConnecting: false,
    gatewayError: null,
    peers: [],
    activePeerId: null,
});
export function addPeer(peer) {
    setConnection("peers", (prev) => {
        if (prev.some((p) => p.peerId === peer.peerId))
            return prev;
        return [...prev, peer];
    });
    // Auto-select if first peer
    if (!connection.activePeerId) {
        setConnection("activePeerId", peer.peerId);
    }
}
export function removePeer(peerId) {
    setConnection("peers", (prev) => prev.filter((p) => p.peerId !== peerId));
    if (connection.activePeerId === peerId) {
        setConnection("activePeerId", connection.peers[0]?.peerId ?? null);
    }
}
export function setActivePeer(peerId) {
    setConnection("activePeerId", peerId);
}
/** Short name from hex peer id (first 6 chars) */
export function shortName(peerId) {
    return peerId.slice(0, 6);
}
export function resetRoom() {
    setConnection({ peers: [], activePeerId: null });
}
export { connection, setConnection };
