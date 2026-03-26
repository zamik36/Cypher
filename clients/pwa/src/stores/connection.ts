import { createStore } from "solid-js/store";
import { setActivePeerForTransfer } from "../api";

export interface PeerInfo {
  peerId: string;
  roomCode: string;
  role: "host" | "guest";
  displayName: string;
}

interface ConnectionState {
  connected: boolean;
  peerId: string | null;
  gatewayAddr: string;
  status: string;
  gatewayConnecting: boolean;
  gatewayError: string | null;
  peers: PeerInfo[];
  activePeerId: string | null;
}

const [connection, setConnection] = createStore<ConnectionState>({
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

export function addPeer(peer: PeerInfo) {
  setConnection("peers", (prev) => {
    if (prev.some((p) => p.peerId === peer.peerId)) return prev;
    return [...prev, peer];
  });
  if (!connection.activePeerId) {
    setConnection("activePeerId", peer.peerId);
    setActivePeerForTransfer(peer.peerId);
  }
}

export function removePeer(peerId: string) {
  setConnection("peers", (prev) => prev.filter((p) => p.peerId !== peerId));
  if (connection.activePeerId === peerId) {
    const next = connection.peers[0]?.peerId ?? null;
    setConnection("activePeerId", next);
    setActivePeerForTransfer(next);
  }
}

export function setActivePeer(peerId: string) {
  setConnection("activePeerId", peerId);
  setActivePeerForTransfer(peerId);
}

export function shortName(peerId: string): string {
  return peerId.slice(0, 6);
}

export function resetRoom() {
  setConnection({ peers: [], activePeerId: null });
  setActivePeerForTransfer(null);
}

export { connection, setConnection };
