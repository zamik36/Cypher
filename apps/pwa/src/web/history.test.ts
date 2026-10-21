// @vitest-environment node
import "fake-indexeddb/auto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { Client, Identity, initSync, type Row } from "../wasm/cypher_wasm.js";
import { importHistory, type HistoryDirs } from "./history";
import { applyOps, openDb, scan } from "./idb";

/** The fixture the native import is checked against too (`crates/cypher-client/src/history.rs`). */
const FIXTURE = resolve(import.meta.dirname, "../../../../crates/cypher-client/tests/fixtures/history_import.txt");

const unhex = (h: string) => Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));
const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

interface Fixture {
  mnemonic: string;
  archive: Uint8Array<ArrayBuffer>;
  before: [string, string, string][];
  after: [string, string, string][];
}

function readFixture(): Fixture {
  const fx: Fixture = { mnemonic: "", archive: new Uint8Array(), before: [], after: [] };
  for (const line of readFileSync(FIXTURE, "utf-8").split("\n")) {
    if (line === "" || line.startsWith("#")) continue;
    const [first = "", ...rest] = line.split(" ");
    if (first === "mnemonic") fx.mnemonic = rest.join(" ");
    else if (first === "archive") fx.archive = unhex(rest[0] ?? "");
    else if (first === "before" || first === "after") fx[first].push([rest[0] ?? "", rest[1] ?? "", rest[2] ?? ""]);
  }
  return fx;
}

/** An OPFS directory held in memory, with only what the worker's history code calls. */
class MemoryDir {
  readonly files = new Map<string, Uint8Array<ArrayBuffer>>();

  getFileHandle(name: string, options?: { create?: boolean }) {
    if (!this.files.has(name)) {
      if (!options?.create) return Promise.reject(new DOMException(name, "NotFoundError"));
      this.files.set(name, new Uint8Array());
    }
    const bytes = () => this.files.get(name) ?? new Uint8Array();
    const set = (b: Uint8Array<ArrayBuffer>) => this.files.set(name, b);
    return Promise.resolve({
      getFile: () => Promise.resolve(new File([bytes()], name)),
      createSyncAccessHandle: () =>
        Promise.resolve({
          truncate: (len: number) => {
            const next = new Uint8Array(len);
            next.set(bytes().subarray(0, len));
            set(next);
          },
          write: (data: Uint8Array, { at }: { at: number }) => {
            const end = Math.max(bytes().length, at + data.length);
            const next = new Uint8Array(end);
            next.set(bytes());
            next.set(data, at);
            set(next);
            return data.length;
          },
          flush: () => undefined,
          close: () => undefined,
        }),
    });
  }
}

let fx: Fixture;
let client: Client;
let identity: Identity;

describe("importHistory", () => {
  beforeAll(() => {
    initSync({ module: readFileSync(resolve(import.meta.dirname, "../wasm/cypher_wasm_bg.wasm")) });
    fx = readFixture();
    identity = Identity.import(fx.mnemonic, "fixture", "a passphrase for the fixture").intoIdentity();
    client = new Client(identity, { meta: [], peers: [], sessions: [], outbox: [], transfers: [] }, Date.now());
  }, 60_000);
  afterAll(() => {
    client.free();
    identity.free();
  });

  it("takes in the shared archive as the native client does", async () => {
    const db = await openDb("history-fixture");
    const history = new MemoryDir();
    const media = new MemoryDir();
    for (const [what, key, value] of fx.before) {
      if (what === "note") media.files.set(key, unhex(value));
      else await applyOps(db, [{ kind: "put", table: what, key: unhex(key), value: unhex(value) }]);
    }
    history.files.set("archive", fx.archive);
    const dirs = { history, media } as unknown as HistoryDirs;

    await importHistory(db, client, dirs, "archive");

    const expected = (what: string) =>
      fx.after
        .filter(([w]) => w === what)
        .map(([, key, value]) => [key, value])
        .sort(([a = ""], [b = ""]) => a.localeCompare(b));
    const stored = (rows: Row[]) => rows.map(([key, value]) => [hex(key), hex(value)]);
    for (const table of ["messages", "message_status", "media"]) {
      expect(stored(await scan(db, table)), table).toEqual(expected(table));
    }
    const notes = [...media.files]
      .map(([id, bytes]) => [id, hex(bytes)])
      .sort(([a = ""], [b = ""]) => a.localeCompare(b));
    expect(notes).toEqual(expected("note"));
    db.close();
  });
});
