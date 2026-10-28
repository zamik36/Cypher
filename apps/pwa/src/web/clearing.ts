/** Clearing the message history in the worker. */

/** The OPFS folders whose files only the history leads to. */
export const HISTORY_DIRS = ["media", "downloads"] as const;

/** The part of an OPFS folder clearing uses. */
export interface Folder {
  keys(): AsyncIterable<string>;
  removeEntry(name: string, options?: { recursive?: boolean }): Promise<void>;
}

/** The part of the OPFS root clearing uses. */
export interface Root {
  getDirectoryHandle(name: string): Promise<Folder>;
}

/** What clearing needs from the worker; it runs in the worker's queue. */
export interface Clearing {
  /**
   * Cancels the transfers still writing into `HISTORY_DIRS` (received files
   * and notes): an unfinished one's file is deleted as it closes.
   */
  cancelWriting(): Promise<void>;
  /** Deletes every message, status and note key row. */
  clearRows(): Promise<void>;
  root(): Promise<Root>;
}

/**
 * Deletes every message, then the sealed copies of notes and the copies of
 * received files kept in OPFS: the user cannot see OPFS, and without their
 * messages nothing leads to them any more. Transfers still writing there
 * are cancelled first, so none finishes later into a file nothing names.
 * (Deleting one conversation or message keeps received files, as the
 * native client keeps them on disk in every case: there they sit in the
 * user's own downloads folder.)
 */
export async function clearHistory(c: Clearing): Promise<void> {
  await c.cancelWriting();
  await c.clearRows();
  const root = await c.root();
  await Promise.all(HISTORY_DIRS.map((dir) => emptyFolder(root, dir)));
}

/** Whether `e` is the DOM failure called `name`. */
const isDomError = (e: unknown, name: string) => e instanceof DOMException && e.name === name;

/**
 * Removes the files in `dir` one by one. One still locked (a write that
 * raced the cancel) stays; any other failure fails the clearing.
 */
async function emptyFolder(root: Root, dir: string): Promise<void> {
  let folder: Folder;
  try {
    folder = await root.getDirectoryHandle(dir);
  } catch (e) {
    // Never made: nothing was kept there.
    if (isDomError(e, "NotFoundError")) return;
    throw e;
  }
  const names: string[] = [];
  for await (const name of folder.keys()) names.push(name);
  await Promise.all(
    names.map((name) =>
      folder.removeEntry(name, { recursive: true }).catch((e: unknown) => {
        if (!isDomError(e, "NoModificationAllowedError")) throw e;
      }),
    ),
  );
}
