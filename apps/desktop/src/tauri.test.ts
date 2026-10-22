import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const emit = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ emit, listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-notification", () => ({}));
vi.mock("@tauri-apps/plugin-process", () => ({}));
vi.mock("@tauri-apps/plugin-updater", () => ({}));

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
