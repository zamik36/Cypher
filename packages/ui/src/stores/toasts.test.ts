import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { addToast, removeToast, toastError, toasts } from "./toasts";

describe("toasts", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("shows a toast for four seconds", () => {
    addToast("hello");
    addToast("boom", "error");
    expect(toasts.map((t) => [t.message, t.type])).toEqual([
      ["hello", "info"],
      ["boom", "error"],
    ]);
    vi.advanceTimersByTime(3999);
    expect(toasts).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(toasts).toHaveLength(0);
  });

  it("shows failures in the user's language", () => {
    toastError(new Error("Cancelled"));
    toastError("something unknown");
    expect(toasts.map((t) => [t.message, t.type])).toEqual([
      ["Cancelled.", "error"],
      ["something unknown", "error"],
    ]);
    vi.advanceTimersByTime(4000);
  });

  it("removes a toast on demand", () => {
    addToast("bye");
    const id = toasts[0]?.id ?? -1;
    removeToast(id);
    expect(toasts).toHaveLength(0);
  });
});
