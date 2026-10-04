import { createStore } from "solid-js/store";

export interface PeerInfo {
  peerId: string;
  roomCode: string;
  role: "host" | "guest";
  /** Short display name derived from peerId */
  displayName: string;
  online: boolean;
}

/**
 * The server connection as the user is told about it.
 * - `idle`: not started (identity locked).
 * - `connecting`: started, the server has not accepted us yet.
 * - `online`: signed in (`Ready`); only now are messages sent directly.
 * - `reconnecting`: was online, retrying with backoff.
 * - `failed`: could not start (bad address, locked identity).
 * - `superseded`: another device signed in with this identity; stays until
 *   the user takes the session back.
 * - `update_required`: the server speaks another protocol; stays.
 */
export type LinkState = "idle" | "connecting" | "online" | "reconnecting" | "failed" | "superseded" | "update_required";

/** What happened: a core event, or the user (re)starting the connection. */
export type LinkEvent = "start" | "connected" | "disconnected" | "failed" | "superseded" | "update_required";

/** States the client left for good; only the user's action ends them. */
const STOPPED: readonly LinkState[] = ["superseded", "update_required"];

export function nextLink(state: LinkState, event: LinkEvent): LinkState {
  switch (event) {
    case "start":
      return state === "update_required" ? state : "connecting";
    case "connected":
      return "online";
    case "disconnected":
      if (STOPPED.includes(state)) return state;
      return state === "online" ? "reconnecting" : state;
    case "failed":
    case "superseded":
    case "update_required":
      return event;
  }
}

interface ConnectionState {
  link: LinkState;
  /** Why the connection could not start, when `link` is `failed`. */
  linkError: string | null;
  peerId: string | null;
  gatewayAddr: string;
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

const initialGatewayAddr = normalizeGatewayAddr(localStorage.getItem(GATEWAY_STORAGE_KEY) || DEFAULT_GATEWAY_ADDR);

localStorage.setItem(GATEWAY_STORAGE_KEY, initialGatewayAddr);

const [connection, setConnection] = createStore<ConnectionState>({
  link: "idle",
  linkError: null,
  peerId: null,
  gatewayAddr: initialGatewayAddr,
  peers: [],
  activePeerId: null,
});

/** The text of a rejected platform call: Tauri rejects with a string, the worker with an `Error`. */
export const errorMessage = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function linkEvent(event: LinkEvent, error: string | null = null) {
  setConnection({ link: nextLink(connection.link, event), linkError: event === "failed" ? error : null });
}

export const isOnline = () => connection.link === "online";

/**
 * Starts (or restarts) the client against the saved address. The state
 * becomes `online` only when the core reports the server accepted us.
 */
export async function connectGateway(start: () => Promise<unknown>): Promise<void> {
  linkEvent("start");
  try {
    await start();
  } catch (e) {
    linkEvent("failed", errorMessage(e));
  }
}

export function addPeer(peer: PeerInfo) {
  setConnection("peers", (prev) =>
    prev.some((p) => p.peerId === peer.peerId)
      ? prev.map((p) => (p.peerId === peer.peerId ? { ...p, online: peer.online, displayName: peer.displayName } : p))
      : [...prev, peer],
  );
  // Auto-select if first peer
  if (!connection.activePeerId) {
    setConnection("activePeerId", peer.peerId);
  }
}

export function setPeerOnline(peerId: string, online: boolean) {
  setConnection("peers", (prev) => prev.map((p) => (p.peerId === peerId ? { ...p, online } : p)));
}

export function markAllPeersOffline() {
  setConnection("peers", (prev) => prev.map((p) => ({ ...p, online: false })));
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

export { connection, setConnection };
