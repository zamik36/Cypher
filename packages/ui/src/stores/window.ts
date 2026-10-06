import { createSignal } from "solid-js";

const CLOSE_TO_TRAY_KEY = "cypher-close-to-tray";

/** Whether closing the window keeps the app in the tray (desktop); on by default. */
const [closeToTray, setCloseToTraySignal] = createSignal(localStorage.getItem(CLOSE_TO_TRAY_KEY) !== "0");

export function setCloseToTray(on: boolean): void {
  setCloseToTraySignal(on);
  localStorage.setItem(CLOSE_TO_TRAY_KEY, on ? "1" : "0");
}

export { closeToTray };
