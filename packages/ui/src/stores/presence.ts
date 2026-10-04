import { createSignal } from "solid-js";

const isActive = () => document.visibilityState === "visible" && document.hasFocus();

const [windowActive, setWindowActive] = createSignal(isActive());
const update = () => setWindowActive(isActive());
document.addEventListener("visibilitychange", update);
window.addEventListener("focus", update);
window.addEventListener("blur", update);

/** Whether the user can see the app right now: visible and focused. */
export { windowActive };
