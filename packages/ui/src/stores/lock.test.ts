import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const load = () => import("./lock");

describe("app lock", () => {
  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
    vi.useFakeTimers();
  });
  afterEach(() => vi.useRealTimers());

  it("is off by default and remembers its settings", async () => {
    let lock = await load();
    expect(lock.autoLock()).toBe(0);
    expect(lock.lockOnHide()).toBe(false);
    lock.setAutoLock(5);
    lock.setLockOnHide(true);
    vi.resetModules();
    lock = await load();
    expect(lock.autoLock()).toBe(5);
    expect(lock.lockOnHide()).toBe(true);
    localStorage.setItem("cypher-autolock", "7");
    vi.resetModules();
    expect((await load()).autoLock()).toBe(0);
  });

  it("locks after the chosen idle time, which input restarts", async () => {
    const { setAutoLock, watchForLock } = await load();
    setAutoLock(1);
    const locked = vi.fn();
    const stop = watchForLock(locked);
    vi.advanceTimersByTime(50_000);
    window.dispatchEvent(new KeyboardEvent("keydown"));
    vi.advanceTimersByTime(50_000);
    expect(locked).not.toHaveBeenCalled();
    vi.advanceTimersByTime(11_000);
    expect(locked).toHaveBeenCalledOnce();
    stop();
    vi.advanceTimersByTime(120_000);
    expect(locked).toHaveBeenCalledOnce();
  });

  it("locks when hidden only if asked, and on request", async () => {
    const { setLockOnHide, watchForLock, lockNow } = await load();
    const locked = vi.fn();
    const stop = watchForLock(locked);
    const hide = (state: DocumentVisibilityState) => {
      vi.spyOn(document, "visibilityState", "get").mockReturnValue(state);
      document.dispatchEvent(new Event("visibilitychange"));
    };
    hide("hidden");
    expect(locked).not.toHaveBeenCalled();
    setLockOnHide(true);
    hide("visible");
    hide("hidden");
    expect(locked).toHaveBeenCalledOnce();
    lockNow();
    expect(locked).toHaveBeenCalledTimes(2);
    stop();
    lockNow();
    expect(locked).toHaveBeenCalledTimes(2);
  });
});
