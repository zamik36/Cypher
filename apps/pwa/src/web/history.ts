/**
 * The message history archive in the worker: built from the stores into an
 * OPFS file for a newly linked device, or taken in from one. Both go a page
 * of rows and a slice of file at a time, so a long history never sits in
 * memory.
 */
import { HistoryWriter, type Client, type HistoryTable, type Row } from "../wasm/cypher_wasm.js";
import { get, insertMissing, scan } from "./idb";

/** Rows read or stored at a time. */
const PAGE = 256;
/** Bytes of a file read at a time. */
const SLICE = 1024 * 1024;

/** Where the archives and the notes live. */
export interface HistoryDirs {
  history: FileSystemDirectoryHandle;
  media: FileSystemDirectoryHandle;
}

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

/** Every row of `table` in key order, a page at a time. */
async function* rows(db: IDBDatabase, table: HistoryTable): AsyncGenerator<Row> {
  let after: Uint8Array | undefined;
  for (;;) {
    const page = await scan(db, table, after ? IDBKeyRange.lowerBound(after, true) : undefined, PAGE);
    yield* page;
    const last = page.at(-1);
    if (!last || page.length < PAGE) return;
    after = last[0];
  }
}

/** The note of a media row as kept here, when it is whole. */
async function wholeNote(media: FileSystemDirectoryHandle, fileId: string, len: number) {
  const note = await media
    .getFileHandle(fileId)
    .then((h) => h.getFile())
    .catch(() => undefined);
  return note?.size === len ? note : undefined;
}

/**
 * Writes the archive `fileId`: every message and status row, and each voice
 * or video note whose copy here is whole and not still on its way
 * (`partial`). Returns its size.
 */
export async function buildHistory(
  db: IDBDatabase,
  client: Client,
  dirs: HistoryDirs,
  fileId: string,
  partial: string[],
): Promise<number> {
  const handle = await (await dirs.history.getFileHandle(fileId, { create: true })).createSyncAccessHandle();
  const writer = new HistoryWriter();
  try {
    handle.truncate(0);
    let at = 0;
    const write = (bytes: Uint8Array) => {
      at += handle.write(bytes, { at });
    };
    write(HistoryWriter.header());
    for await (const [key, value] of rows(db, "media")) {
      const id = hex(key);
      const len = partial.includes(id) ? undefined : client.noteLen(id, value);
      const note = len === undefined ? undefined : await wholeNote(dirs.media, id, len);
      const row = note && writer.row("media", key, value);
      if (!note || !row) continue;
      write(row);
      write(writer.blob(id, note.size));
      for (let from = 0; from < note.size; from += SLICE) {
        write(new Uint8Array(await note.slice(from, from + SLICE).arrayBuffer()));
      }
    }
    for (const table of ["messages", "message_status"] as const) {
      for await (const [key, value] of rows(db, table)) {
        const row = writer.row(table, key, value);
        if (row) write(row);
      }
    }
    write(writer.end());
    handle.flush();
    return at;
  } finally {
    writer.free();
    handle.close();
  }
}

/**
 * Takes in the archive `fileId` as the core's import says: the rows these
 * stores do not have yet, each only if this profile sealed it, and the
 * notes of the media rows, each stored before its row.
 */
export async function importHistory(db: IDBDatabase, client: Client, dirs: HistoryDirs, fileId: string): Promise<void> {
  const archive = await (await dirs.history.getFileHandle(fileId)).getFile();
  const reader = client.historyReader();
  let table: HistoryTable | undefined;
  let pending: Row[] = [];
  const store = async (to: HistoryTable, row?: Row) => {
    if (table !== to || !row || pending.length >= PAGE) {
      if (table) await insertMissing(db, table, pending);
      pending = [];
      table = to;
    }
    if (row) pending.push(row);
  };
  /** The note being written; none when this device keeps it already. */
  let note: { handle: FileSystemSyncAccessHandle; at: number } | undefined;
  try {
    for (let from = 0; from < archive.size; from += SLICE) {
      reader.push(new Uint8Array(await archive.slice(from, from + SLICE).arrayBuffer()));
      for (let steps = reader.next(); steps; steps = reader.next()) {
        for (const step of steps) {
          switch (step.kind) {
            case "row":
              await store(step.table, [step.key, step.value]);
              break;
            case "note_start": {
              // A row here means the note is here, whole: it was stored first.
              if ((await get(db, "media", step.key)) !== undefined) break;
              const file = await dirs.media.getFileHandle(step.file_id, { create: true });
              note = { handle: await file.createSyncAccessHandle(), at: 0 };
              note.handle.truncate(0);
              break;
            }
            case "note_data":
              if (note) note.at += note.handle.write(step.data, { at: note.at });
              break;
            case "note_end":
              note?.handle.flush();
              note?.handle.close();
              note = undefined;
              await store("media", [step.key, step.value]);
              break;
          }
        }
      }
    }
    reader.finish();
    await store("messages");
  } finally {
    note?.handle.close();
    reader.free();
  }
}
