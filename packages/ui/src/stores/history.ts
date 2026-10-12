import { createSignal } from "solid-js";
import type { HistoryState } from "../platform";

/** The message history on its way to or from this device; `null` when none is. */
const [historyTransfer, setHistoryTransfer] = createSignal<HistoryState | null>(null);

export { historyTransfer, setHistoryTransfer };

/** How far along it is, in whole percent. */
export function historyPercent(): number {
  return Math.round((historyTransfer()?.progress ?? 0) * 100);
}
