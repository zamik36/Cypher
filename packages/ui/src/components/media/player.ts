/** Only one voice or video note plays at a time, as in every messenger. */
let current: HTMLMediaElement | null = null;

export function claimPlayback(el: HTMLMediaElement) {
  if (current && current !== el) current.pause();
  current = el;
}

export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}
