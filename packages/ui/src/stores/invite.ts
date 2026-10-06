import { push } from "./nav";
import { findInvite } from "../utils/invite";

let ready = false;
let pending: string | null = null;

function showJoin(code: string): void {
  push({ name: "new-chat", tab: "join", code });
}

/**
 * An invite that reached the app from outside (a link opened, a deep link):
 * shown in New chat once the app is unlocked, at once if it already is.
 */
export function receiveInvite(text: string): void {
  const code = findInvite(text);
  if (!code) return;
  if (ready) showJoin(code);
  else pending = code;
}

/** Whether the app can show screens now (unlocked); a waiting invite shows. */
export function setInvitesReady(on: boolean): void {
  ready = on;
  if (on && pending) {
    showJoin(pending);
    pending = null;
  }
}
