import "fake-indexeddb/auto";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { Row } from "../wasm/cypher_wasm.js";
import { applyOps, clear, get, insertMissing, messageRows, openDb, put, remove, scan } from "./idb";

const key = (...b: number[]) => new Uint8Array(b);
const rows = (list: [Uint8Array, Uint8Array][]) => list.map(([k, v]) => [[...k], [...v]]);
let db: IDBDatabase;

describe("idb", () => {
  beforeEach(async () => {
    db = await openDb("test");
  });
  afterEach(() => {
    db.close();
    indexedDB.deleteDatabase("test");
  });

  it("adds only the rows whose keys are free, keeping what is stored", async () => {
    await applyOps(db, [{ kind: "put", table: "messages", key: key(2), value: key(20) }]);
    await insertMissing(db, "messages", [
      [key(1), key(1)],
      [key(2), key(99)],
      [key(3), key(3)],
    ]);
    await insertMissing(db, "messages", []);
    expect(rows(await scan(db, "messages"))).toEqual([
      [[1], [1]],
      [[2], [20]],
      [[3], [3]],
    ]);
  });

  it("pages through a table after a given key", async () => {
    await insertMissing(
      db,
      "media",
      [1, 2, 3, 4].map((n): Row => [key(n), key(n)]),
    );
    expect(rows(await scan(db, "media", IDBKeyRange.lowerBound(key(2), true), 2))).toEqual([
      [[3], [3]],
      [[4], [4]],
    ]);
  });

  it("creates every store", () => {
    expect([...db.objectStoreNames]).toContain("message_status");
  });

  it("applies a batch across tables atomically", async () => {
    await applyOps(db, [
      { kind: "put", table: "meta", key: key(1), value: key(10) },
      { kind: "put", table: "peers", key: key(2), value: key(20) },
      { kind: "put", table: "meta", key: key(3), value: key(30) },
    ]);
    await applyOps(db, [{ kind: "delete", table: "meta", key: key(1) }]);
    await applyOps(db, []);
    expect(rows(await scan(db, "meta"))).toEqual([[[3], [30]]]);
    expect(rows(await scan(db, "peers"))).toEqual([[[2], [20]]]);
  });

  it("rolls the whole batch back when one operation fails", async () => {
    await expect(
      applyOps(db, [
        { kind: "put", table: "meta", key: key(1), value: key(1) },
        { kind: "put", table: "missing", key: key(2), value: key(2) },
      ]),
    ).rejects.toThrow();
    expect(await scan(db, "meta")).toEqual([]);
  });

  it("scans a key range, newest first, up to a limit", async () => {
    await applyOps(
      db,
      [1, 2, 3, 4, 5].map((i) => ({ kind: "put" as const, table: "messages", key: key(7, i), value: key(i) })),
    );
    const range = IDBKeyRange.bound(key(7, 2), key(7, 5), false, true);
    expect(rows(await scan(db, "messages", range)).map(([k]) => k)).toEqual([
      [7, 2],
      [7, 3],
      [7, 4],
    ]);
    expect(rows(await scan(db, "messages", range, 2, true)).map(([k]) => k)).toEqual([
      [7, 4],
      [7, 3],
    ]);
  });

  it("reads the latest messages of several ranges with their statuses in one go", async () => {
    await applyOps(db, [
      ...[1, 2, 3].flatMap((peer) =>
        [1, 2, 3].map((i) => ({ kind: "put" as const, table: "messages", key: key(peer, i), value: key(peer, i) })),
      ),
      { kind: "put", table: "message_status", key: key(2), value: key(42) },
    ]);
    const span = (peer: number) => IDBKeyRange.bound(key(peer), key(peer, 9));
    // The status row of a message is named by its key's last byte here.
    const statusKey = (k: Uint8Array<ArrayBuffer>) => k.subarray(k.length - 1);
    const got = await messageRows(db, [span(1), span(3), span(5)], 2, statusKey);
    const shown = got.map((list) => list.map(([k, , s]) => [[...k], s ? [...s] : null]));
    expect(shown).toEqual([
      [
        [[1, 3], null],
        [[1, 2], [42]],
      ],
      [
        [[3, 3], null],
        [[3, 2], [42]],
      ],
      [],
    ]);
  });

  it("stores, reads, removes and clears plain values", async () => {
    await put(db, "identity", "current", key(1, 2));
    await put(db, "files", "f", "blob");
    // The structured clone lives in another realm; compare the bytes.
    expect([...((await get<Uint8Array>(db, "identity", "current")) ?? [])]).toEqual([1, 2]);
    await remove(db, "identity", "current");
    expect(await get(db, "identity", "current")).toBeUndefined();
    await clear(db, ["files"]);
    expect(await get(db, "files", "f")).toBeUndefined();
  });
});
