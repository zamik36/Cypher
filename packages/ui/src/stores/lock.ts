import { createSignal } from "solid-js";

/** Minutes of no input before the app locks itself; 0 never does. */
export const AUTO_LOCK_CHOICES = [0, 1, 5, 15, 60] as const;
export type AutoLock = (typeof AUTO_LOCK_CHOICES)[number];

const MINUTES_KEY = "cypher-autolock";
const ON_HIDE_KEY = "cypher-lock-on-hide";

function storedMinutes(): AutoLock {
  const saved = Number(localStorage.getItem(MINUTES_KEY));
  return AUTO_LOCK_CHOICES.find((m) => m === saved) ?? 0;
}

const [autoLock, setAutoLockSignal] = createSignal<AutoLock>(storedMinutes());
const [lockOnHide, setLockOnHideSignal] = createSignal(localStorage.getItem(ON_HIDE_KEY) === "1");

export function setAutoLock(minutes: AutoLock): void {
  setAutoLockSignal(minutes);
  localStorage.setItem(MINUTES_KEY, String(minutes));
}

export function setLockOnHide(on: boolean): void {
  setLockOnHideSignal(on);
  localStorage.setItem(ON_HIDE_KEY, on ? "1" : "0");
}

let handler: (() => void) | undefined;

/** Locks the app now (the app registers how, see `watchForLock`). */
export function lockNow(): void {
  handler?.();
}

/**
 * Locks after `autoLock()` minutes without a key or a tap, and when the
 * app is hidden if `lockOnHide()`. Returns the cleanup.
 */
export function watchForLock(lock: () => void): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const arm = () => {
    clearTimeout(timer);
    const minutes = autoLock();
    if (minutes > 0) timer = setTimeout(lock, minutes * 60_000);
  };
  const onVisibility = () => {
    if (document.visibilityState === "hidden" && lockOnHide()) lock();
    else arm();
  };
  handler = lock;
  arm();
  window.addEventListener("pointerdown", arm, { capture: true, passive: true });
  window.addEventListener("keydown", arm, { capture: true, passive: true });
  document.addEventListener("visibilitychange", onVisibility);
  return () => {
    clearTimeout(timer);
    if (handler === lock) handler = undefined;
    window.removeEventListener("pointerdown", arm, { capture: true });
    window.removeEventListener("keydown", arm, { capture: true });
    document.removeEventListener("visibilitychange", onVisibility);
  };
}

export { autoLock, lockOnHide };
