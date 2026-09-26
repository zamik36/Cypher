import { createStore } from "solid-js/store";

export interface PeerInfo {
  peerId: string;
  roomCode: string;
  role: "host" | "guest";
  /** Short display name derived from peerId */
  displayName: string;
  online: boolean;
  inboxId?: string | null;
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

const DEFAULT_GATEWAY_ADDR = "cyphermessanger.tech:9100";
const GATEWAY_STORAGE_KEY = "cypher-gateway";

export function normalizeGatewayAddr(raw: string): string {
  let value = raw.trim();
  if (!value) {
    return DEFAULT_GATEWAY_ADDR;
  }

  value = value.replace(/^[a-z]+:\/\//i, "");
  value = value.split(/[/?#]/, 1)[0] ?? "";

  if (!value) {
    return DEFAULT_GATEWAY_ADDR;
  }

  if (value.startsWith("[")) {
    return /\]:\d+$/.test(value) ? value : `${value}:9100`;
  }

  return /:\d+$/.test(value) ? value : `${value}:9100`;
}

const initialGatewayAddr = normalizeGatewayAddr(
  localStorage.getItem(GATEWAY_STORAGE_KEY) || DEFAULT_GATEWAY_ADDR,
);

localStorage.setItem(GATEWAY_STORAGE_KEY, initialGatewayAddr);

const [connection, setConnection] = createStore<ConnectionState>({
  connected: false,
  peerId: null,
  gatewayAddr: initialGatewayAddr,
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
      updated[idx] = {
        ...updated[idx],
        online: peer.online,
        displayName: peer.displayName,
        inboxId: peer.inboxId ?? updated[idx].inboxId ?? null,
      };
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

export function setPeerInboxId(peerId: string, inboxId: string | null) {
  setConnection("peers", (prev) =>
    prev.map((p) => p.peerId === peerId ? { ...p, inboxId } : p),
  );
}

export function markAllPeersOffline() {
  setConnection("peers", (prev) => prev.map((p) => ({ ...p, online: false })));
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

export function setGatewayAddr(addr: string): string {
  const normalized = normalizeGatewayAddr(addr);
  setConnection("gatewayAddr", normalized);
  localStorage.setItem(GATEWAY_STORAGE_KEY, normalized);
  return normalized;
}

/** Short name from hex peer id (first 6 chars) */
export function shortName(peerId: string): string {
  return peerId.slice(0, 6);
}

export function resetRoom() {
  setConnection({ peers: [], activePeerId: null });
}

export { connection, setConnection };
