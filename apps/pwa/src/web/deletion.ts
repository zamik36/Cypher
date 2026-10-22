/** Deleting stored messages in the worker, as the core says each one goes. */
import type { MessageDeletion, Op, Row } from "../wasm/cypher_wasm.js";

/** What deleting needs from the worker. */
export interface Deleting {
  /** What deleting one message row takes (the core's `message_deletion`). */
  deletion(key: Uint8Array<ArrayBuffer>, value: Uint8Array<ArrayBuffer>): MessageDeletion;
  /** Applies a batch of store operations atomically. */
  apply(ops: Op[]): Promise<void>;
  /** Removes the sealed copy of a voice or video note; one already gone is fine. */
  removeNote(fileId: string): Promise<void>;
}

/**
 * Deletes stored messages with their statuses and the sealed copies of their
 * voice and video notes. Files received stay in `downloads`: deleting a
 * conversation never deletes the user's files.
 */
export async function deleteRows(rows: Row[], d: Deleting): Promise<void> {
  const notes: string[] = [];
  const ops: Op[] = [];
  for (const [key, value] of rows) {
    const deletion = d.deletion(key, value);
    ops.push(...deletion.ops);
    if (deletion.note) notes.push(deletion.note);
  }
  await d.apply(ops);
  await Promise.all(notes.map((id) => d.removeNote(id)));
}
