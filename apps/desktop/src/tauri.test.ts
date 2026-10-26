import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const emit = vi.fn();
const relaunch = vi.fn();
const check = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ emit, listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-notification", () => ({}));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check }));

const { tauriPlatform } = await import("./tauri");
const { CommandError } = await import("@cypher/ui/errors");

const file = (name: string) => new File([name], name, { type: "text/plain" });

/** The name each `send_bytes` call carried. */
const sentNames = () =>
  invoke.mock.calls.map((call) => {
    const options = call[2] as { headers: Record<string, string> };
    return decodeURIComponent(options.headers["x-name"] ?? "");
  });

describe("sendFiles", () => {
  beforeEach(() => {
    invoke.mockReset();
    emit.mockReset();
  });

  it("keeps the cards of the files sent when another one fails", async () => {
    invoke.mockImplementation((_cmd: string, _body: unknown, options: { headers: Record<string, string> }) => {
      const name = decodeURIComponent(options.headers["x-name"] ?? "");
      return name === "b.txt" ? Promise.reject(new Error("too large")) : Promise.resolve({ file_name: name });
    });
    const sent = await tauriPlatform.sendFiles("peer", [file("a.txt"), file("b.txt"), file("c.txt")]);
    expect(sent).toEqual([{ file_name: "a.txt" }, { file_name: "c.txt" }]);
    expect(sentNames()).toEqual(["a.txt", "b.txt", "c.txt"]);
    // Told to the UI as the native side tells it about a batch's leftovers.
    expect(emit).toHaveBeenCalledWith("cypher://file_not_sent", {
      name: "b.txt",
      error: { kind: "internal", message: "too large" },
    });
  });

  it("keeps the cards when the UI cannot be told of a file left out", async () => {
    invoke.mockRejectedValueOnce(new Error("too large")).mockResolvedValueOnce({ file_name: "b.txt" });
    emit.mockRejectedValue(new Error("no listener"));
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const sent = await tauriPlatform.sendFiles("peer", [file("a.txt"), file("b.txt")]);
    expect(sent).toEqual([{ file_name: "b.txt" }]);
  });

  it("fails with the first error when no file went", async () => {
    invoke.mockRejectedValueOnce(new Error("first")).mockRejectedValueOnce(new Error("second"));
    await expect(tauriPlatform.sendFiles("peer", [file("a.txt"), file("b.txt")])).rejects.toThrow("first");
    expect(emit).not.toHaveBeenCalled();
  });
});

describe("commands", () => {
  beforeEach(() => invoke.mockReset());

  it("fail with the command's kind and reason", async () => {
    invoke.mockRejectedValueOnce({ kind: "failed", reason: "offline", message: "failed: Offline" });
    const joined = tauriPlatform.joinLink("link");
    await expect(joined).rejects.toBeInstanceOf(CommandError);
    await expect(joined).rejects.toMatchObject({ kind: "failed", reason: "offline", message: "failed: Offline" });
  });

  it("fail as internal errors with Tauri's own text", async () => {
    invoke.mockRejectedValueOnce("invalid args `peerId` for command `send_message`");
    await expect(tauriPlatform.sendMessage("peer", "hi")).rejects.toMatchObject({
      kind: "internal",
      message: "invalid args `peerId` for command `send_message`",
    });
  });
});

describe("updates", () => {
  /** An update whose steps, and the commands around them, go into `trace`. */
  function offer(trace: string[]) {
    const step = (name: string) => () => {
      trace.push(name);
      return Promise.resolve();
    };
    check.mockResolvedValue({
      version: "9.9.9",
      download: vi.fn(step("download")),
      install: vi.fn(step("install")),
      downloadAndInstall: vi.fn(step("downloadAndInstall")),
    });
    invoke.mockImplementation((cmd: string) => step(`invoke ${cmd}`)());
    relaunch.mockImplementation(step("relaunch"));
  }

  it("stops the client before installing, as the Windows installer exits the app itself", async () => {
    const trace: string[] = [];
    offer(trace);
    expect(await tauriPlatform.updates?.check()).toBe("9.9.9");
    await tauriPlatform.updates?.install();
    expect(trace).toEqual(["download", "invoke lock", "install", "relaunch"]);
  });

  it("starts the app over when installing fails after the client stopped", async () => {
    const trace: string[] = [];
    offer(trace);
    await tauriPlatform.updates?.check();
    const update = (await check.mock.results[0]?.value) as { install: ReturnType<typeof vi.fn> };
    update.install.mockRejectedValueOnce(new Error("installer gone"));
    await expect(tauriPlatform.updates?.install()).rejects.toThrow("installer gone");
    expect(trace).toEqual(["download", "invoke lock", "relaunch"]);
  });
});
