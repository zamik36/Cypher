/** Deleting stored messages in the worker, as the core says each one goes. */
import type { MessageDeletion, Op, Row } from "../wasm/cypher_wasm.js";
import type { Sink } from "./effects";

/** What deleting needs from the worker. */
export interface Deleting {
  /** What deleting one message row takes (the core's `message_deletion`). */
  deletion(key: Uint8Array<ArrayBuffer>, value: Uint8Array<ArrayBuffer>): MessageDeletion;
  /** Applies a batch of store operations atomically. */
  apply(ops: Op[]): Promise<void>;
  /** Removes a file kept in OPFS; one already gone is fine. */
  remove(sink: Sink, fileId: string): Promise<void>;
}

/** Deletes stored messages with their statuses, media notes and kept files. */
export async function deleteRows(rows: Row[], d: Deleting): Promise<void> {
  const notes: string[] = [];
  const received: string[] = [];
  const ops: Op[] = [];
  for (const [key, value] of rows) {
    const deletion = d.deletion(key, value);
    ops.push(...deletion.ops);
    if (deletion.note) notes.push(deletion.note);
    if (deletion.file) received.push(deletion.file);
  }
  await d.apply(ops);
  await Promise.all([...notes.map((id) => d.remove("media", id)), ...received.map((id) => d.remove("download", id))]);
}
