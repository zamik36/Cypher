import { isConnecting } from "../utils/reasons";

/**
 * Work that failed only because the desktop's client was still starting
 * (`connecting`) runs again once the client is up (`client_ready`). An
 * attempt notes the starts seen when it began (`readyMark`): if a start has
 * finished since, its event is already gone, so the work runs again at once
 * instead of waiting for one.
 */
let starts = 0;
let waiting: (() => void)[] = [];

/** The client is up: what waited for it runs. */
export function clientReady(): void {
  starts++;
  waiting.splice(0).forEach((again) => again());
}

/** Taken when an attempt begins, for `retryWhenReady`. */
export function readyMark(): number {
  return starts;
}

/**
 * Runs `again` once the client is up when `error` says it is starting, and
 * returns `true`; returns `false` for any other error.
 */
export function retryWhenReady(error: unknown, mark: number, again: () => void): boolean {
  if (!isConnecting(error)) return false;
  if (starts !== mark) again();
  else waiting.push(again);
  return true;
}

/** Locking drops what waited: it belonged to the identity that left. */
export function dropRetries(): void {
  waiting = [];
}
