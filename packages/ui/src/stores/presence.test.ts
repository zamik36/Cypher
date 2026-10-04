import { afterEach, describe, expect, it, vi } from "vitest";

describe("windowActive", () => {
  afterEach(() => vi.resetModules());

  it("follows focus and visibility", async () => {
    const focus = vi.spyOn(document, "hasFocus").mockReturnValue(true);
    const { windowActive } = await import("./presence");
    expect(windowActive()).toBe(true);

    focus.mockReturnValue(false);
    window.dispatchEvent(new Event("blur"));
    expect(windowActive()).toBe(false);

    focus.mockReturnValue(true);
    window.dispatchEvent(new Event("focus"));
    expect(windowActive()).toBe(true);

    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    expect(windowActive()).toBe(false);
  });
});
