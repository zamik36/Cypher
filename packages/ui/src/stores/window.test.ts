import { beforeEach, describe, expect, it, vi } from "vitest";

describe("window behaviour", () => {
  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
  });

  it("keeps the app in the tray by default, and remembers otherwise", async () => {
    let window = await import("./window");
    expect(window.closeToTray()).toBe(true);
    window.setCloseToTray(false);
    vi.resetModules();
    window = await import("./window");
    expect(window.closeToTray()).toBe(false);
  });
});
