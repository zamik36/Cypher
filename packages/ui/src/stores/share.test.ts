import { beforeEach, describe, expect, it, vi } from "vitest";

describe("shares from other apps", () => {
  beforeEach(() => vi.resetModules());

  it("wait for the unlock, go to the chosen chat once, and die with a lock", async () => {
    const share = await import("./share");
    const file = new File(["hi"], "a.txt", { type: "text/plain" });
    share.receiveShare({ files: [], text: "  " });
    share.receiveShare({ files: [file], text: "" });
    expect(share.pendingShare()).toBeNull();
    share.setSharesReady(true);
    expect(share.pendingShare()?.files).toEqual([file]);

    share.shareWith("bob");
    expect(share.pendingShare()).toBeNull();
    expect(share.takeShare("carol")).toBeNull();
    expect(share.takeShare("bob")?.files).toEqual([file]);
    expect(share.takeShare("bob")).toBeNull();

    share.receiveShare({ files: [], text: "a link" });
    share.cancelShare();
    expect(share.pendingShare()).toBeNull();
    share.shareWith("bob");
    expect(share.takeShare("bob")).toBeNull();

    share.receiveShare({ files: [], text: "a link" });
    share.setSharesReady(false);
    share.setSharesReady(true);
    expect(share.pendingShare()).toBeNull();
  });
});
