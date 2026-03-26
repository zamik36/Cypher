import { createStore } from "solid-js/store";
import { setActivePeerForTransfer } from "../api";
const [connection, setConnection] = createStore({
    connected: false,
    peerId: null,
    gatewayAddr: location.protocol === "https:"
        ? `${location.host}/ws`
        : `${location.hostname}:9101`,
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
    if (!connection.activePeerId) {
        setConnection("activePeerId", peer.peerId);
        setActivePeerForTransfer(peer.peerId);
    }
}
export function removePeer(peerId) {
    setConnection("peers", (prev) => prev.filter((p) => p.peerId !== peerId));
    if (connection.activePeerId === peerId) {
        const next = connection.peers[0]?.peerId ?? null;
        setConnection("activePeerId", next);
        setActivePeerForTransfer(next);
    }
}
export function setActivePeer(peerId) {
    setConnection("activePeerId", peerId);
    setActivePeerForTransfer(peerId);
}
export function shortName(peerId) {
    return peerId.slice(0, 6);
}
export function resetRoom() {
    setConnection({ peers: [], activePeerId: null });
    setActivePeerForTransfer(null);
}
export { connection, setConnection };
