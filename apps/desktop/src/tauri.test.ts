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
    expect(emit).toHaveBeenCalledWith("cypher://error", "b.txt: too large");
  });

  it("fails with the first error when no file went", async () => {
    invoke.mockRejectedValueOnce(new Error("first")).mockRejectedValueOnce(new Error("second"));
    await expect(tauriPlatform.sendFiles("peer", [file("a.txt"), file("b.txt")])).rejects.toThrow("first");
    expect(emit).not.toHaveBeenCalled();
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
