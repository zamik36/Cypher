/** Offering a file the user picked or recorded, and keeping its bytes until its transfer ends. */

/** What offering a file needs from the worker. */
export interface Offering {
  /** Runs a `send_file` command through the core: the ids it went out with. */
  run(cmd: Record<string, unknown>): Promise<{ msgId?: string; fileId?: string }>;
  /** Keeps `file` as the source of transfer `fileId`. */
  keep(fileId: string, file: File): Promise<void>;
}

/**
 * Offers a file: `cmd` is its `send_file` command, `file` its bytes unless
 * they travel inline. Resolves with the ids it went out with, or nothing
 * when the core gave none.
 */
export async function offerFile(
  o: Offering,
  cmd: Record<string, unknown>,
  file: File | undefined,
): Promise<{ msgId: string; fileId: string } | undefined> {
  const { msgId, fileId } = await o.run(cmd);
  if (!msgId || !fileId) return undefined;
  if (file) await o.keep(fileId, file);
  return { msgId, fileId };
}
