import { describe, expect, it } from "vitest";
import type { Effect } from "../wasm/cypher_wasm.js";
import { applyEffects, type EffectSinks } from "./effects";

const bytes = (...b: number[]) => new Uint8Array(b);

/** Sinks that record every call in order; persistence completes asynchronously. */
function recorder() {
  const log: string[] = [];
  const later = (entry: string) =>
    new Promise<void>((resolve) =>
      setTimeout(() => {
        log.push(entry);
        resolve();
      }, 0),
    );
  const sinks: EffectSinks = {
    persist: (ops) => later(`persist ${ops.map((o) => `${o.kind}:${o.table}`).join(",")}`),
    transmit: (data) => log.push(`transmit ${data.join(".")}`),
    anonymous: (data) => log.push(`anonymous ${data.join(".")}`),
    event: (channel) => log.push(`event ${channel}`),
    reply: (r) => log.push(`reply ${r.op}`),
    readChunk: (r) => log.push(`read ${r.file_id}#${r.index}`),
    openSink: (id, len, sealed) => later(`open ${id} ${len} ${sealed}`),
    writeChunk: (id, offset) => log.push(`write ${id}@${offset}`),
    closeSink: (id, complete) => later(`close ${id} ${complete}`),
    disconnect: (reconnect) => log.push(`disconnect ${reconnect}`),
  };
  return { log, sinks };
}

describe("applyEffects", () => {
  it("makes a storage batch durable before anything leaves the device", async () => {
    const { log, sinks } = recorder();
    const effects: Effect[] = [
      { kind: "put", table: "outbox", key: bytes(1), value: bytes(2) },
      { kind: "delete", table: "peers", key: bytes(3) },
      { kind: "transmit", data: bytes(9) },
      { kind: "put", table: "meta", key: bytes(4), value: bytes(5) },
      { kind: "anonymous", data: bytes(8) },
      { kind: "put", table: "meta", key: bytes(6), value: bytes(7) },
    ];
    await applyEffects(effects, sinks);
    expect(log).toEqual([
      "persist put:outbox,delete:peers",
      "transmit 9",
      "persist put:meta",
      "anonymous 8",
      "persist put:meta",
    ]);
  });

  it("sends nothing after a storage batch fails", async () => {
    const { log, sinks } = recorder();
    sinks.persist = () => Promise.reject(new DOMException("quota", "QuotaExceededError"));
    await expect(
      applyEffects(
        [
          { kind: "put", table: "outbox", key: bytes(1), value: bytes(2) },
          { kind: "transmit", data: bytes(9) },
          { kind: "anonymous", data: bytes(8) },
        ],
        sinks,
      ),
    ).rejects.toThrow("quota");
    expect(log).toEqual([]);
  });

  it("routes every other effect in order without persisting empty batches", async () => {
    const { log, sinks } = recorder();
    await applyEffects(
      [
        { kind: "event", channel: "connected", payload: null },
        { kind: "reply", op: "joined", value: "peer" },
        { kind: "read_chunk", file_id: "f", index: 2, offset: 0, len: 4, headroom: 0 },
        { kind: "open_sink", file_id: "f", len: 10, sealed: true },
        { kind: "write_chunk", file_id: "f", offset: 4, data: bytes(1) },
        { kind: "close_sink", file_id: "f", complete: false },
        { kind: "transmit", data: bytes(1) },
        { kind: "disconnect", reconnect: true },
      ],
      sinks,
    );
    expect(log).toEqual([
      "event connected",
      "reply joined",
      "read f#2",
      "open f 10 true",
      "write f@4",
      "close f false",
      "transmit 1",
      "disconnect true",
    ]);
  });
});
