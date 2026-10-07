import { createSignal } from "solid-js";
import { api } from "../platform";
import { notificationsEnabled } from "../utils/notifications";

const WANTED_KEY = "cypher-push";

/** Where waking this device while the app is closed stands. */
export type PushState = "off" | "pending" | "registered" | "unavailable";

/** The user's choice; on unless turned off. It needs notifications too. */
const [pushWanted, setPushWantedSignal] = createSignal(localStorage.getItem(WANTED_KEY) !== "0");
const [pushState, setPushState] = createSignal<PushState>("off");

export { pushState, pushWanted };

export const pushSupported = (): boolean => api.push !== undefined;

const active = () => pushWanted() && notificationsEnabled();

/** Asks for wake-ups when the user wants them (on every start, which also renews them). */
export async function startPush(): Promise<void> {
  if (!api.push || !active()) return;
  setPushState("pending");
  await api.push.enable().catch(() => setPushState("unavailable"));
}

/** The server's key arrived: subscribe this device with it. */
export async function onPushKey(key: string): Promise<void> {
  if (!api.push || !active()) return;
  const subscribed = await api.push.subscribe(key).catch(() => false);
  if (!subscribed) setPushState("unavailable");
}

export function onPushState(state: "registered" | "unavailable"): void {
  if (active()) setPushState(state);
}

/** Stops wake-ups on this device and on the server. */
export async function stopPush(): Promise<void> {
  setPushState("off");
  await api.push?.disable().catch(() => undefined);
}

export async function setPushWanted(on: boolean): Promise<void> {
  setPushWantedSignal(on);
  localStorage.setItem(WANTED_KEY, on ? "1" : "0");
  await (on ? startPush() : stopPush());
}
