import { createSignal } from "solid-js";

export type SettingsSection = "profile" | "appearance" | "notifications" | "privacy" | "storage";

/** Every screen the app can show; the chat list is always at the bottom. */
export type Screen =
  | { name: "chats" }
  | { name: "chat"; peerId: string }
  | { name: "new-chat"; tab: "invite" | "join"; code?: string }
  | { name: "contact"; peerId: string }
  | { name: "settings" }
  | { name: "settings-section"; section: SettingsSection };

const ROOT: Screen = { name: "chats" };

/**
 * The screen stack mirrors the browser history (`{ depth }` in its state),
 * so the Android back button, browser back and Escape all go through one
 * path: `history.back()`, then `popstate` trims the stack.
 */
const [stack, setStack] = createSignal<readonly Screen[]>([ROOT]);

export const screens = stack;
export const top = (): Screen => stack()[stack().length - 1] ?? ROOT;
export const depth = () => stack().length - 1;

/** The innermost open screen named `name`, if any. */
export function find<N extends Screen["name"]>(name: N): Extract<Screen, { name: N }> | undefined {
  return [...stack()].reverse().find((s): s is Extract<Screen, { name: N }> => s.name === name);
}

export function push(screen: Screen): void {
  setStack([...stack(), screen]);
  history.pushState({ depth: depth() }, "");
}

/** Swaps the top screen without adding a history entry (the root stays). */
export function replace(screen: Screen): void {
  if (depth() === 0) {
    push(screen);
    return;
  }
  setStack([...stack().slice(0, -1), screen]);
}

/** Goes back one screen; does nothing on the chat list. */
export function back(): void {
  if (depth() > 0) history.back();
}

/** Applies a history move: keeps the screens up to `targetDepth`. */
export function popTo(targetDepth: number): void {
  if (targetDepth < depth()) setStack(stack().slice(0, Math.max(targetDepth, 0) + 1));
}

/** Drops every screen above the chat list (e.g. after a sign out). */
export function reset(): void {
  setStack([ROOT]);
  history.replaceState({ depth: 0 }, "");
}

function depthOf(state: unknown): number {
  const value = (state as { depth?: unknown } | null)?.depth;
  return typeof value === "number" ? value : 0;
}

function onKey(e: KeyboardEvent): void {
  // An open <dialog> handles Escape itself (its `cancel` event).
  if (e.key !== "Escape" || e.defaultPrevented || document.querySelector("dialog[open]")) return;
  if (depth() > 0) {
    e.preventDefault();
    back();
  }
}

/** Wires history and Escape once, at startup. Returns the cleanup. */
export function installNavigation(): () => void {
  history.replaceState({ depth: 0 }, "");
  const onPop = (e: PopStateEvent) => popTo(depthOf(e.state));
  window.addEventListener("popstate", onPop);
  window.addEventListener("keydown", onKey);
  return () => {
    window.removeEventListener("popstate", onPop);
    window.removeEventListener("keydown", onKey);
  };
}
