import { describe, expect, it } from "vitest";
import { mediaProgress, mediaReady, setMediaProgress, trackMedia } from "./media";

describe("media progress", () => {
  it("treats untracked notes as stored and ready", () => {
    expect(mediaReady("history")).toBe(true);
    expect(mediaProgress("history")).toBe(1);
  });

  it("tracks a new note until it completes, capping progress at 1", () => {
    trackMedia("n1");
    expect(mediaReady("n1")).toBe(false);
    setMediaProgress("n1", 0.4);
    expect(mediaProgress("n1")).toBe(0.4);
    setMediaProgress("n1", 1.7);
    expect(mediaProgress("n1")).toBe(1);
    expect(mediaReady("n1")).toBe(true);
  });

  it("does not reset a note that already completed", () => {
    trackMedia("n2");
    setMediaProgress("n2", 1);
    trackMedia("n2");
    expect(mediaReady("n2")).toBe(true);
  });
});
