import { afterEach, describe, expect, it, vi } from "vitest";
import { CommandError } from "@cypher/ui/errors";
import { Replies } from "./replies";

afterEach(() => vi.useRealTimers());

/** Sends nothing, only notes the id the command went with. */
function sender(ids: number[]) {
  return (id: number) => {
    ids.push(id);
    return Promise.resolve();
  };
}

describe("Replies", () => {
  it("answers each command by its own id, whatever came first", async () => {
    const replies = new Replies(1000);
    const ids: number[] = [];
    const first = replies.request(sender(ids), "joined");
    const second = replies.request(sender(ids), "joined");
    await Promise.resolve();
    const [a = 0, b = 0] = ids;
    replies.answer({ kind: "reply", op: "joined", id: b, value: "bob" });
    replies.answer({ kind: "reply", op: "join_failed", id: a, value: "not_found" });
    await expect(second).resolves.toBe("bob");
    await expect(first).rejects.toMatchObject({ kind: "failed", reason: "not_found" });
  });

  it("fails a command whose send threw, and one never answered", async () => {
    vi.useFakeTimers();
    const replies = new Replies(1000);
    const refused = replies.request(() => Promise.reject(new Error("not connected")), "link_created");
    await expect(refused).rejects.toMatchObject({ kind: "internal", message: "not connected" });
    const silent = replies.request(sender([]), "link_created");
    const timedOut = expect(silent).rejects.toMatchObject({ kind: "timed_out" });
    await vi.advanceTimersByTimeAsync(1000);
    await timedOut;
  });

  it("fails every command still waiting at once", async () => {
    const replies = new Replies(1000);
    const waiting = [replies.request(sender([]), "joined"), replies.request(sender([]), "device_linked")];
    replies.failAll(new CommandError({ kind: "locked", message: "identity is locked" }));
    for (const w of waiting) await expect(w).rejects.toMatchObject({ kind: "locked" });
  });
});
