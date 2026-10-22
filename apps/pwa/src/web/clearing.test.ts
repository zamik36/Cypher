import { describe, expect, it } from "vitest";
import { clearHistory, type Root } from "./clearing";

/** An OPFS root holding `files` by folder; `locked` ones cannot be removed (still being written). */
function fakeRoot(files: Record<string, string[]>, locked: string[] = []) {
  const left = new Map(Object.entries(files).map(([dir, names]) => [dir, new Set(names)]));
  const removeAll = (dir: string) => {
    const names = left.get(dir);
    if (!names) return Promise.reject(new DOMException("missing", "NotFoundError"));
    if ([...names].some((n) => locked.includes(n))) {
      return Promise.reject(new DOMException("locked", "NoModificationAllowedError"));
    }
    left.delete(dir);
    return Promise.resolve();
  };
  const root: Root = {
    removeEntry: (name, options) => (options?.recursive ? removeAll(name) : Promise.reject(new Error("not empty"))),
  };
  return { root, left };
}

describe("clearHistory", () => {
  it("deletes the rows first, then the notes and received files kept in OPFS", async () => {
    const trace: string[] = [];
    const { root, left } = fakeRoot({ media: ["n1"], downloads: ["f1", "f2"], history: ["h1"] });
    await clearHistory({
      clearRows: () => {
        trace.push(`rows, ${left.size} folders`);
        return Promise.resolve();
      },
      root: () => Promise.resolve(root),
    });
    expect(trace).toEqual(["rows, 3 folders"]);
    // An archive on its way to or from another device is not history.
    expect([...left.keys()]).toEqual(["history"]);
  });
});
