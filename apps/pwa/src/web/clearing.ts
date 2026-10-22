/** Clearing the message history in the worker. */

/** The OPFS folders whose files only the history leads to. */
export const HISTORY_DIRS = ["media", "downloads"] as const;

/** The part of the OPFS root clearing uses. */
export interface Root {
  removeEntry(name: string, options?: { recursive?: boolean }): Promise<void>;
}

/** What clearing needs from the worker. */
export interface Clearing {
  /** Deletes every message, status and note key row. */
  clearRows(): Promise<void>;
  root(): Promise<Root>;
}

/**
 * Deletes every message, then the sealed copies of notes and the copies of
 * received files kept in OPFS: the user cannot see OPFS, and without their
 * messages nothing leads to them any more. (Deleting one conversation or
 * message keeps received files, as the native client keeps them on disk in
 * every case: there they sit in the user's own downloads folder.)
 */
export async function clearHistory(c: Clearing): Promise<void> {
  await c.clearRows();
  const root = await c.root();
  await Promise.all(HISTORY_DIRS.map((dir) => root.removeEntry(dir, { recursive: true }).catch(() => undefined)));
}
