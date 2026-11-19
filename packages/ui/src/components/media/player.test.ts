import { describe, expect, it, vi } from "vitest";
import { claimPlayback, formatDuration } from "./player";

describe("formatDuration", () => {
  it.each([
    [0, "0:00"],
    [-500, "0:00"],
    [1499, "0:01"],
    [59_600, "1:00"],
    [61_000, "1:01"],
    [3_600_000, "60:00"],
  ])("%d ms → %s", (ms, text) => {
    expect(formatDuration(ms)).toBe(text);
  });
});

describe("claimPlayback", () => {
  it("pauses the previous player only", () => {
    const a = document.createElement("audio");
    const b = document.createElement("audio");
    const pauseA = vi.spyOn(a, "pause").mockImplementation(() => undefined);
    const pauseB = vi.spyOn(b, "pause").mockImplementation(() => undefined);
    claimPlayback(a);
    claimPlayback(a);
    expect(pauseA).not.toHaveBeenCalled();
    claimPlayback(b);
    expect(pauseA).toHaveBeenCalledOnce();
    expect(pauseB).not.toHaveBeenCalled();
  });
});
