/** Offering a file the user picked or recorded, and keeping its bytes until its transfer ends. */

/** What offering a file needs from the worker. */
export interface Offering {
  /** Runs a `send_file` command through the core and applies its effects, the offer going out among them. */
  run(cmd: Record<string, unknown>): Promise<{ msgId?: string; fileId?: string }>;
  /** Keeps `file` durably as the source of transfer `fileId`. */
  keep(fileId: string, file: File): Promise<void>;
  /** Lets go of what `keep` kept. */
  drop(fileId: string): Promise<void>;
}

/** A fresh file id, as the core's hex. */
function newFileId(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * Offers a file: `cmd` is its `send_file` command, `file` its bytes unless
 * they travel inline. The bytes are kept before the command runs, under the
 * id it then goes out with: once the offer has left, this device can serve
 * it after a restart too, as the native client can. Resolves with the ids,
 * or nothing when the core gave none.
 */
export async function offerFile(
  o: Offering,
  cmd: Record<string, unknown>,
  file: File | undefined,
): Promise<{ msgId: string; fileId: string } | undefined> {
  let kept: string | undefined;
  if (file) {
    kept = newFileId();
    await o.keep(kept, file);
  }
  let ids: { msgId?: string; fileId?: string };
  try {
    ids = await o.run(kept === undefined ? cmd : { ...cmd, file_id: kept });
  } catch (e) {
    if (kept !== undefined) await o.drop(kept);
    throw e;
  }
  const { msgId, fileId } = ids;
  if (!msgId || !fileId) {
    if (kept !== undefined) await o.drop(kept);
    return undefined;
  }
  return { msgId, fileId };
}
