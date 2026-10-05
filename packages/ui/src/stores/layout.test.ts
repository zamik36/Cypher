import { beforeEach, describe, expect, it, vi } from "vitest";

describe("layout", () => {
  beforeEach(() => vi.resetModules());

  it("follows the width media query", async () => {
    let listener: ((e: { matches: boolean }) => void) | undefined;
    const matchMedia = vi.fn((query: string) => ({
      matches: false,
      media: query,
      addEventListener: (_: string, cb: (e: { matches: boolean }) => void) => (listener = cb),
    }));
    vi.stubGlobal("matchMedia", matchMedia);
    const { isWide, WIDE_MIN_PX } = await import("./layout");
    expect(matchMedia).toHaveBeenCalledWith(`(min-width: ${WIDE_MIN_PX}px)`);
    expect(isWide()).toBe(false);
    listener?.({ matches: true });
    expect(isWide()).toBe(true);
  });

  it("is narrow without matchMedia", async () => {
    vi.stubGlobal("matchMedia", undefined);
    const { isWide } = await import("./layout");
    expect(isWide()).toBe(false);
  });
});
