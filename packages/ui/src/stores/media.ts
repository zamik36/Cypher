import { createStore } from "solid-js/store";

/**
 * Download progress of voice and video notes (0..1). Absent means the note
 * came from history and is already stored; 1 means it is ready to play.
 */
const [progress, setProgress] = createStore<Record<string, number>>({});

/** Marks a new note as not yet playable, unless it already completed. */
export function trackMedia(fileId: string) {
  if (progress[fileId] !== 1) setProgress(fileId, 0);
}

export function setMediaProgress(fileId: string, value: number) {
  setProgress(fileId, Math.min(1, value));
}

export function mediaReady(fileId: string): boolean {
  const p = progress[fileId];
  return p === undefined || p === 1;
}

export function mediaProgress(fileId: string): number {
  return progress[fileId] ?? 1;
}
