import { describe, expect, it } from "vitest";
import { clearHistory, type Root } from "./clearing";

/** An OPFS root holding `files` by folder; `locked` ones cannot be removed (still being written). */
function fakeRoot(files: Record<string, string[]>, locked: string[] = []) {
  const left = new Map(Object.entries(files).map(([dir, names]) => [dir, new Set(names)]));
  const refuse = (name: string) => Promise.reject(new DOMException(name, "NoModificationAllowedError"));
  const folder = (dir: string) => {
    const names = left.get(dir);
    if (!names) return Promise.reject(new DOMException(dir, "NotFoundError"));
    return Promise.resolve({
      keys: async function* () {
        // A copy: removing while listing must not skip any.
        for (const name of [...names]) yield await Promise.resolve(name);
      },
      removeEntry: (name: string) => {
        if (locked.includes(name)) return refuse(name);
        names.delete(name);
        return Promise.resolve();
      },
    });
  };
  const root: Root = {
    getDirectoryHandle: folder,
    removeEntry: (dir, options) => {
      const names = left.get(dir);
      if (!names) return Promise.reject(new DOMException(dir, "NotFoundError"));
      if (!options?.recursive || [...names].some((n) => locked.includes(n))) return refuse(dir);
      left.delete(dir);
      return Promise.resolve();
    },
  };
  /** The files left, as `folder/name`. */
  const remaining = () => [...left].flatMap(([dir, names]) => [...names].map((n) => `${dir}/${n}`));
  return { root, remaining };
}

describe("clearHistory", () => {
  it("deletes the rows first, then the notes and received files kept in OPFS", async () => {
    const trace: string[] = [];
    const { root, remaining } = fakeRoot({ media: ["n1"], downloads: ["f1", "f2"], history: ["h1"] });
    await clearHistory({
      clearRows: () => {
        trace.push(`rows, ${remaining().length} files`);
        return Promise.resolve();
      },
      root: () => Promise.resolve(root),
    });
    expect(trace).toEqual(["rows, 4 files"]);
    // An archive on its way to or from another device is not history.
    expect(remaining()).toEqual(["history/h1"]);
  });

  it("deletes every received file but one still being written", async () => {
    const { root, remaining } = fakeRoot({ media: ["n1"], downloads: ["f1", "f2", "f3"] }, ["f2"]);
    await clearHistory({ clearRows: () => Promise.resolve(), root: () => Promise.resolve(root) });
    expect(remaining()).toEqual(["downloads/f2"]);
  });

  it("finds nothing to delete in folders never made", async () => {
    const { root, remaining } = fakeRoot({ history: ["h1"] });
    await clearHistory({ clearRows: () => Promise.resolve(), root: () => Promise.resolve(root) });
    expect(remaining()).toEqual(["history/h1"]);
  });
});
