import { beforeEach, describe, expect, it, vi } from "vitest";

describe("retrying once the desktop's client is up", () => {
  beforeEach(() => vi.resetModules());

  it("waits for the client when it is starting, and only then", async () => {
    const ready = await import("./clientReady");
    const again = vi.fn();
    expect(ready.retryWhenReady("not connected", ready.readyMark(), again)).toBe(false);
    expect(ready.retryWhenReady(new Error("connecting"), ready.readyMark(), again)).toBe(true);
    expect(again).not.toHaveBeenCalled();
    ready.clientReady();
    expect(again).toHaveBeenCalledOnce();
    ready.clientReady();
    expect(again).toHaveBeenCalledOnce();
  });

  it("runs at once when the client came up while the attempt was under way", async () => {
    const ready = await import("./clientReady");
    const again = vi.fn();
    const mark = ready.readyMark();
    // The event came before the failure did.
    ready.clientReady();
    expect(ready.retryWhenReady("connecting", mark, again)).toBe(true);
    expect(again).toHaveBeenCalledOnce();
  });

  it("forgets what waited when the app locks", async () => {
    const ready = await import("./clientReady");
    const again = vi.fn();
    ready.retryWhenReady("connecting", ready.readyMark(), again);
    ready.dropRetries();
    ready.clientReady();
    expect(again).not.toHaveBeenCalled();
  });
});
