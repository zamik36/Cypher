import { createStore } from "solid-js/store";

export interface PeerInfo {
  peerId: string;
  roomCode: string;
  role: "host" | "guest";
  /** Short display name derived from peerId */
  displayName: string;
  online: boolean;
}

interface ConnectionState {
  connected: boolean;
  peerId: string | null;
  gatewayAddr: string;
  status: string;
  gatewayConnecting: boolean;
  gatewayError: string | null;
  /** All connected peers */
  peers: PeerInfo[];
  /** Currently active chat peer */
  activePeerId: string | null;
}

const [connection, setConnection] = createStore<ConnectionState>({
  connected: false,
  peerId: null,
  gatewayAddr: localStorage.getItem("cypher-gateway") || "cyphermessanger.tech:9100",
  status: "disconnected",
  gatewayConnecting: false,
  gatewayError: null,
  peers: [],
  activePeerId: null,
});

export function addPeer(peer: PeerInfo) {
  setConnection("peers", (prev) => {
    const idx = prev.findIndex((p) => p.peerId === peer.peerId);
    if (idx >= 0) {
      const updated = [...prev];
      updated[idx] = { ...updated[idx], online: peer.online, displayName: peer.displayName };
      return updated;
    }
    return [...prev, peer];
  });
  // Auto-select if first peer
  if (!connection.activePeerId) {
    setConnection("activePeerId", peer.peerId);
  }
}

export function setPeerOnline(peerId: string, online: boolean) {
  setConnection("peers", (prev) =>
    prev.map((p) => p.peerId === peerId ? { ...p, online } : p),
  );
}

export function removePeer(peerId: string) {
  setConnection("peers", (prev) => prev.filter((p) => p.peerId !== peerId));
  if (connection.activePeerId === peerId) {
    setConnection("activePeerId", connection.peers[0]?.peerId ?? null);
  }
}

export function setActivePeer(peerId: string) {
  setConnection("activePeerId", peerId);
}

/** Short name from hex peer id (first 6 chars) */
export function shortName(peerId: string): string {
  return peerId.slice(0, 6);
}

export function resetRoom() {
  setConnection({ peers: [], activePeerId: null });
}

// Persist gateway address changes
const originalSetConnection = setConnection;

export { connection, setConnection };
