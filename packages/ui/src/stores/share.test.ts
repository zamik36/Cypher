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

  it("send each file once, the rest after a client that was still starting is up", async () => {
    const share = await import("./share");
    const ready = await import("./clientReady");
    const files = ["a", "b", "c"].map((name) => new File([name], `${name}.txt`));
    const tried: string[] = [];
    let starting = false;
    const send = (file: File) => {
      tried.push(file.name);
      if (starting) return Promise.reject(new Error("connecting"));
      // The client restarts once, after the first file.
      if (tried.length === 1) starting = true;
      return Promise.resolve([{ file_name: file.name } as never]);
    };
    const sent: string[] = [];
    const failed = vi.fn();
    share.sendEach(files, send, (cards) => sent.push(...cards.map((c: { file_name: string }) => c.file_name)), failed);
    await vi.waitFor(() => expect(tried).toEqual(["a.txt", "b.txt"]));

    starting = false;
    ready.clientReady();
    await vi.waitFor(() => expect(sent).toEqual(["a.txt", "b.txt", "c.txt"]));
    expect(tried).toEqual(["a.txt", "b.txt", "b.txt", "c.txt"]);
    expect(failed).not.toHaveBeenCalled();
  });

  it("go on past a file that fails for another reason", async () => {
    const share = await import("./share");
    const files = ["a", "b"].map((name) => new File([name], `${name}.txt`));
    const send = (file: File) =>
      file.name === "a.txt"
        ? Promise.reject(new Error("too large"))
        : Promise.resolve([{ file_name: file.name } as never]);
    const sent = vi.fn();
    const failed = vi.fn();
    share.sendEach(files, send, sent, failed);
    await vi.waitFor(() => expect(sent).toHaveBeenCalledOnce());
    expect(failed).toHaveBeenCalledWith(new Error("too large"));
  });
});
