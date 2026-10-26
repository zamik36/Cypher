/** Commands the worker waits on for their answer, told apart by id. */
import { CommandError, type FailReason, toCommandError } from "@cypher/ui/errors";
import type { Reply } from "../wasm/cypher_wasm.js";

/** The core failed the command, for `reason`. */
export const failed = (reason: string) =>
  new CommandError({ kind: "failed", reason: reason as FailReason, message: `failed: ${reason}` });

/** Commands waiting for the reply that answers them. */
export class Replies {
  private readonly waiting = new Map<number, (r: Reply | CommandError) => void>();
  private next = 1;

  constructor(private readonly timeoutMs: number) {}

  /**
   * Sends the command with `send`, under a fresh id, and resolves with the
   * value of the reply that answers it when that reply is `ok`; any other
   * reply is why it failed. Replies to other commands do not touch it. A
   * `send` that throws fails it with that error.
   */
  async request(send: (id: number) => Promise<unknown>, ok: Reply["op"]): Promise<string> {
    const id = this.next++;
    const reply = new Promise<string>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.delete(id);
        reject(new CommandError({ kind: "timed_out", message: "timed out" }));
      }, this.timeoutMs);
      this.waiting.set(id, (r) => {
        clearTimeout(timer);
        if (r instanceof CommandError) reject(r);
        else if (r.op === ok) resolve(r.value);
        else reject(failed(r.value));
      });
    });
    try {
      await send(id);
    } catch (e) {
      this.settle(id, toCommandError(e));
    }
    return reply;
  }

  /** Hands `reply` to the command it answers, if one waits for it. */
  answer(reply: Reply) {
    this.settle(reply.id, reply);
  }

  /** Fails every command still waiting, with `e`. */
  failAll(e: CommandError) {
    const waiting = [...this.waiting.values()];
    this.waiting.clear();
    for (const waiter of waiting) waiter(e);
  }

  private settle(id: number, outcome: Reply | CommandError) {
    const waiter = this.waiting.get(id);
    this.waiting.delete(id);
    waiter?.(outcome);
  }
}
