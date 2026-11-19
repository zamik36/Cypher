import { describe, expect, it } from "vitest";
import { clearHistory, type Clearing, type Root } from "./clearing";

/**
 * An OPFS root holding `files` by folder. `locked` ones are still being
 * written: removing them fails until their transfer is cancelled, which
 * removes them (an unfinished sink is deleted on close). `broken` ones fail
 * to be removed for another reason.
 */
function fakeRoot(files: Record<string, string[]>, locked: string[] = [], broken: string[] = []) {
  const left = new Map(Object.entries(files).map(([dir, names]) => [dir, new Set(names)]));
  const writing = new Set(locked);
  const refuse = (name: string, kind: string) => Promise.reject(new DOMException(name, kind));
  const folder = (dir: string) => {
    const names = left.get(dir);
    if (!names) return refuse(dir, "NotFoundError");
    return Promise.resolve({
      keys: async function* () {
        // A copy: removing while listing must not skip any.
        for (const name of [...names]) yield await Promise.resolve(name);
      },
      removeEntry: (name: string) => {
        if (writing.has(name)) return refuse(name, "NoModificationAllowedError");
        if (broken.includes(name)) return refuse(name, "InvalidStateError");
        names.delete(name);
        return Promise.resolve();
      },
    });
  };
  const root: Root = { getDirectoryHandle: folder };
  /** The transfers still writing are cancelled: their files go. */
  const cancelWriting = () => {
    for (const names of left.values()) for (const name of writing) names.delete(name);
    writing.clear();
    return Promise.resolve();
  };
  /** The files left, as `folder/name`. */
  const remaining = () => [...left].flatMap(([dir, names]) => [...names].map((n) => `${dir}/${n}`));
  return { root, cancelWriting, remaining };
}

function clearing(fake: ReturnType<typeof fakeRoot>, trace: string[] = []): Clearing {
  return {
    cancelWriting: () => {
      trace.push(`cancel, ${fake.remaining().length} files`);
      return fake.cancelWriting();
    },
    clearRows: () => {
      trace.push("rows");
      return Promise.resolve();
    },
    root: () => Promise.resolve(fake.root),
  };
}

describe("clearHistory", () => {
  it("cancels what is still written, deletes the rows, then the notes and received files", async () => {
    const trace: string[] = [];
    const fake = fakeRoot({ media: ["n1"], downloads: ["f1", "f2"], history: ["h1"] });
    await clearHistory(clearing(fake, trace));
    expect(trace).toEqual(["cancel, 4 files", "rows"]);
    // An archive on its way to or from another device is not history.
    expect(fake.remaining()).toEqual(["history/h1"]);
  });

  it("leaves no received file behind, not even one still being written", async () => {
    const fake = fakeRoot({ media: ["n1"], downloads: ["f1", "f2", "f3"] }, ["f2"]);
    await clearHistory(clearing(fake));
    expect(fake.remaining()).toEqual([]);
  });

  it("fails when a file cannot be deleted for another reason", async () => {
    const fake = fakeRoot({ downloads: ["f1", "f2"] }, [], ["f1"]);
    await expect(clearHistory(clearing(fake))).rejects.toThrow("f1");
    expect(fake.remaining()).toEqual(["downloads/f1"]);
  });

  it("finds nothing to delete in folders never made", async () => {
    const fake = fakeRoot({ history: ["h1"] });
    await clearHistory(clearing(fake));
    expect(fake.remaining()).toEqual(["history/h1"]);
  });
});
