import { createSignal } from "solid-js";

/** From this width the chat list and the open chat sit side by side (see `--list-w`). */
export const WIDE_MIN_PX = 840;

const query = typeof matchMedia === "function" ? matchMedia(`(min-width: ${WIDE_MIN_PX}px)`) : null;
const [wide, setWide] = createSignal(query?.matches ?? false);
query?.addEventListener("change", (e) => setWide(e.matches));

/** Whether the window is wide enough for two columns. */
export const isWide = wide;
