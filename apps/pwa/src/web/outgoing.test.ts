import { describe, expect, it } from "vitest";
import { offerFile, type Offering } from "./outgoing";

/** A core that records what it was asked and stands for the transmit after each command. */
function offering(ids: { msgId?: string; fileId?: string } = { msgId: "m", fileId: "f" }) {
  const trace: string[] = [];
  const cmds: Record<string, unknown>[] = [];
  const o: Offering = {
    run: (cmd) => {
      cmds.push(cmd);
      trace.push("run");
      return Promise.resolve(ids);
    },
    keep: (fileId) => {
      trace.push(`keep ${fileId}`);
      return Promise.resolve();
    },
  };
  return { o, trace, cmds };
}

describe("offerFile", () => {
  it("keeps the bytes of a file that does not travel inline", async () => {
    const { o, trace } = offering();
    const ids = await offerFile(o, { type: "send_file" }, new File(["x"], "x"));
    expect(ids).toEqual({ msgId: "m", fileId: "f" });
    expect(trace).toEqual(["run", "keep f"]);
  });

  it("keeps nothing for inline bytes or a command without ids", async () => {
    const inline = offering();
    expect(await offerFile(inline.o, { type: "send_file" }, undefined)).toEqual({ msgId: "m", fileId: "f" });
    expect(inline.trace).toEqual(["run"]);
    const none = offering({});
    expect(await offerFile(none.o, { type: "send_file" }, new File(["x"], "x"))).toBeUndefined();
    expect(none.trace).toEqual(["run"]);
  });
});
