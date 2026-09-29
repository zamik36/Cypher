import { describe, expect, it, vi } from "vitest";
import { t } from "../i18n";
import { toasts } from "../stores/toasts";
import { copyText } from "./clipboard";

/** jsdom has no Clipboard API; install a stand-in for one test. */
const stubClipboard = (writeText: (text: string) => Promise<void>) =>
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });

describe("copyText", () => {
  it("copies and reports success", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    stubClipboard(writeText);
    await expect(copyText("seed")).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("seed");
  });

  it("turns a refusal into an error toast", async () => {
    stubClipboard(() => Promise.reject(new DOMException("denied", "NotAllowedError")));
    await expect(copyText("seed")).resolves.toBe(false);
    expect(toasts.at(-1)).toMatchObject({ message: t().toast_copy_failed, type: "error" });
  });
});
