import { describe, expect, it } from "vitest";
import type { MessageDeletion, Op, Row } from "../wasm/cypher_wasm.js";
import { deleteRows } from "./deletion";

const bytes = (...b: number[]) => new Uint8Array(b);

/** Rows 1, 2 and 3: a text, a voice note and a received file. */
const rows: Row[] = [1, 2, 3].map((n) => [bytes(n), bytes(n)]);
const deletions: Record<number, MessageDeletion> = {
  1: { ops: [{ kind: "delete", table: "messages", key: bytes(1) }], note: undefined, file: undefined },
  2: { ops: [{ kind: "delete", table: "messages", key: bytes(2) }], note: "n2", file: undefined },
  3: { ops: [{ kind: "delete", table: "messages", key: bytes(3) }], note: undefined, file: "f3" },
};

async function run() {
  const trace: string[] = [];
  let applied: Op[] = [];
  await deleteRows(rows, {
    deletion: (key) => {
      const d = deletions[key[0] ?? 0];
      if (!d) throw new Error("unknown row");
      return d;
    },
    apply: (ops) => {
      trace.push("apply");
      applied = ops;
      return Promise.resolve();
    },
    remove: (sink, fileId) => {
      trace.push(`remove ${sink} ${fileId}`);
      return Promise.resolve();
    },
  });
  return { trace, applied };
}

describe("deleteRows", () => {
  it("deletes every row in one batch, then the sealed copies of notes", async () => {
    const { trace, applied } = await run();
    expect(applied.map((op) => [...op.key])).toEqual([[1], [2], [3]]);
    expect(trace[0]).toBe("apply");
    expect(trace).toContain("remove media n2");
  });

  it("leaves received files where they are, as the native client does", async () => {
    const { trace } = await run();
    expect(trace.filter((t) => t.startsWith("remove download"))).toEqual([]);
  });
});
