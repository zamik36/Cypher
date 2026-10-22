import { describe, expect, it } from "vitest";
import { offerFile, Refused, staleFiles, type Offering } from "./outgoing";

/**
 * A core that records what it was asked: `run` stands for the command and
 * the offer it transmits, so whatever is kept before it is durable before
 * the offer leaves.
 */
function offering(failure?: Error) {
  const trace: string[] = [];
  const cmds: Record<string, unknown>[] = [];
  const o: Offering = {
    run: (cmd) => {
      cmds.push(cmd);
      trace.push("run");
      if (failure) return Promise.reject(failure);
      return Promise.resolve({ msgId: "m", fileId: typeof cmd["file_id"] === "string" ? cmd["file_id"] : "f" });
    },
    keep: (fileId) => {
      trace.push(`keep ${fileId}`);
      return Promise.resolve();
    },
    drop: (fileId) => {
      trace.push(`drop ${fileId}`);
      return Promise.resolve();
    },
  };
  return { o, trace, cmds };
}

const file = () => new File(["x"], "x");

describe("offerFile", () => {
  it("keeps a file's bytes before its offer goes out, under the id it goes out with", async () => {
    const { o, trace, cmds } = offering();
    const ids = await offerFile(o, { type: "send_file" }, file());
    const fileId = cmds[0]?.["file_id"];
    expect(fileId).toMatch(/^[0-9a-f]{32}$/);
    expect(trace).toEqual([`keep ${String(fileId)}`, "run"]);
    expect(ids).toEqual({ msgId: "m", fileId });
  });

  it("keeps nothing for bytes that travel inline", async () => {
    const { o, trace, cmds } = offering();
    expect(await offerFile(o, { type: "send_file" }, undefined)).toEqual({ msgId: "m", fileId: "f" });
    expect(trace).toEqual(["run"]);
    expect(cmds[0]).not.toHaveProperty("file_id");
  });

  it("lets go of the bytes when the core refused the command", async () => {
    const { o, trace, cmds } = offering(new Refused(new Error("invalid peer id")));
    await expect(offerFile(o, { type: "send_file" }, file())).rejects.toThrow("invalid peer id");
    const fileId = String(cmds[0]?.["file_id"]);
    expect(trace).toEqual([`keep ${fileId}`, "run", `drop ${fileId}`]);
  });

  it("keeps the bytes when the command failed after its offer may have gone out", async () => {
    const { o, trace, cmds } = offering(new Error("StorageFailed"));
    await expect(offerFile(o, { type: "send_file" }, file())).rejects.toThrow("StorageFailed");
    expect(trace).toEqual([`keep ${String(cmds[0]?.["file_id"])}`, "run"]);
  });
});

describe("staleFiles", () => {
  it("names kept bytes that no transfer is reading any more", () => {
    const transfer = (hex: string) => Uint8Array.from(hex.match(/../g) ?? [], (x) => parseInt(x, 16));
    const kept = ["aa".repeat(16), "bb".repeat(16), "cc".repeat(16)];
    const transfers = [
      [transfer("bb".repeat(16)), new Uint8Array()] as [Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer>],
    ];
    expect(staleFiles(kept, transfers)).toEqual(["aa".repeat(16), "cc".repeat(16)]);
    expect(staleFiles([], [])).toEqual([]);
  });
});
