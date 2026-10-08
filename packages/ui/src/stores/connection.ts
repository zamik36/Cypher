import { createStore } from "solid-js/store";

/**
 * The server connection as the user is told about it.
 * - `idle`: not started (identity locked).
 * - `connecting`: started, the server has not accepted us yet.
 * - `online`: signed in (`Ready`); only now are messages sent directly.
 * - `reconnecting`: was online, retrying with backoff.
 * - `failed`: could not start (bad address, locked identity).
 * - `superseded`: this device's session opened elsewhere (another window, or
 *   an install restored from the phrase); stays until the user takes it back.
 * - `update_required`: the server speaks another protocol; stays.
 * - `unlinked`: this device was taken off its profile; stays.
 */
export type LinkState =
  "idle" | "connecting" | "online" | "reconnecting" | "failed" | "superseded" | "update_required" | "unlinked";

/** What happened: a core event, or the user (re)starting the connection. */
export type LinkEvent =
  "start" | "connected" | "disconnected" | "failed" | "superseded" | "update_required" | "unlinked";

/** States the client left for good; only the user's action ends them. */
const STOPPED: readonly LinkState[] = ["superseded", "update_required", "unlinked"];

/** States no reconnecting gets out of. */
const FINAL: readonly LinkState[] = ["update_required", "unlinked"];

export function nextLink(state: LinkState, event: LinkEvent): LinkState {
  switch (event) {
    case "start":
      return FINAL.includes(state) ? state : "connecting";
    case "connected":
      return "online";
    case "disconnected":
      if (STOPPED.includes(state)) return state;
      return state === "online" ? "reconnecting" : state;
    case "failed":
    case "superseded":
    case "update_required":
    case "unlinked":
      return event;
  }
}

interface ConnectionState {
  link: LinkState;
  /** Why the connection could not start, when `link` is `failed`. */
  linkError: string | null;
  peerId: string | null;
  gatewayAddr: string;
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

export function setGatewayAddr(addr: string): string {
  const normalized = normalizeGatewayAddr(addr);
  setConnection("gatewayAddr", normalized);
  localStorage.setItem(GATEWAY_STORAGE_KEY, normalized);
  return normalized;
}

export { connection, setConnection };
