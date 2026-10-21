import { describe, expect, it } from "vitest";
import { offerFile, type Offering } from "./outgoing";

/**
 * A core that records what it was asked: `run` stands for the command and
 * the offer it transmits, so whatever is kept before it is durable before
 * the offer leaves.
 */
function offering(fail = false) {
  const trace: string[] = [];
  const cmds: Record<string, unknown>[] = [];
  const o: Offering = {
    run: (cmd) => {
      cmds.push(cmd);
      trace.push("run");
      if (fail) return Promise.reject(new Error("StorageFailed"));
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

describe("offerFile", () => {
  it("keeps a file's bytes before its offer goes out, under the id it goes out with", async () => {
    const { o, trace, cmds } = offering();
    const ids = await offerFile(o, { type: "send_file" }, new File(["x"], "x"));
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

  it("lets go of the bytes when the offer could not be made", async () => {
    const { o, trace, cmds } = offering(true);
    await expect(offerFile(o, { type: "send_file" }, new File(["x"], "x"))).rejects.toThrow("StorageFailed");
    const fileId = String(cmds[0]?.["file_id"]);
    expect(trace).toEqual([`keep ${fileId}`, "run", `drop ${fileId}`]);
  });
});
