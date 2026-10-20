import { describe, expect, it, vi } from "vitest";
import type { Effect } from "../wasm/cypher_wasm.js";
import { applyEffects, EffectApplier, type EffectSinks, StorageFailedError } from "./effects";

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
    openSink: (id, len, sink) => later(`open ${id} ${len} ${sink}`),
    writeChunk: (id, offset) => log.push(`write ${id}@${offset}`),
    closeSink: (id, complete) => later(`close ${id} ${complete}`),
    buildHistory: (id, partial) => log.push(`build ${id} [${partial.join(",")}]`),
    importHistory: (id) => log.push(`import ${id}`),
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

  it("stores what came before an effect it does not know, then refuses it", async () => {
    const { log, sinks } = recorder();
    const unknown = { kind: "launch" } as unknown as Effect;
    const applied = applyEffects([{ kind: "put", table: "meta", key: bytes(1), value: bytes(2) }, unknown], sinks);
    await expect(applied).rejects.toThrow(StorageFailedError);
    await expect(applied).rejects.toThrow("unknown effect");
    expect(log).toEqual(["persist put:meta"]);
  });

  it("stores what came before a sink that failed, and runs nothing after it", async () => {
    const { log, sinks } = recorder();
    sinks.openSink = () => Promise.reject(new Error("no room in OPFS"));
    await expect(
      applyEffects(
        [
          { kind: "put", table: "transfers", key: bytes(1), value: bytes(2) },
          { kind: "open_sink", file_id: "f", len: 1, sink: "download" },
          { kind: "transmit", data: bytes(9) },
        ],
        sinks,
      ),
    ).rejects.toThrow("no room in OPFS");
    expect(log).toEqual(["persist put:transfers"]);
  });

  it("routes every other effect in order without persisting empty batches", async () => {
    const { log, sinks } = recorder();
    await applyEffects(
      [
        { kind: "event", channel: "connected", payload: null },
        { kind: "reply", op: "joined", value: "peer" },
        { kind: "read_chunk", file_id: "f", index: 2, offset: 0, len: 4, headroom: 0 },
        { kind: "open_sink", file_id: "f", len: 10, sink: "media" },
        { kind: "write_chunk", file_id: "f", offset: 4, data: bytes(1) },
        { kind: "close_sink", file_id: "f", complete: false },
        { kind: "build_history", file_id: "h", partial: ["f"] },
        { kind: "import_history", file_id: "h" },
        { kind: "transmit", data: bytes(1) },
        { kind: "disconnect", reconnect: true },
      ],
      sinks,
    );
    expect(log).toEqual([
      "event connected",
      "reply joined",
      "read f#2",
      "open f 10 media",
      "write f@4",
      "close f false",
      "build h [f]",
      "import h",
      "transmit 1",
      "disconnect true",
    ]);
  });
});

describe("EffectApplier", () => {
  const sendAndStore: Effect[] = [
    { kind: "put", table: "outbox", key: bytes(1), value: bytes(2) },
    { kind: "transmit", data: bytes(9) },
    { kind: "event", channel: "message", payload: null },
  ];

  it("applies batches while storage works", async () => {
    const { log, sinks } = recorder();
    const stopped = vi.fn();
    const applier = new EffectApplier(sinks, stopped);
    await applier.apply(sendAndStore);
    await applier.apply(sendAndStore);
    expect(log).toEqual([
      "persist put:outbox",
      "transmit 9",
      "event message",
      "persist put:outbox",
      "transmit 9",
      "event message",
    ]);
    expect(applier.stopped).toBe(false);
    expect(stopped).not.toHaveBeenCalled();
  });

  it("stops for good after a storage batch failed", async () => {
    const { log, sinks } = recorder();
    let storageWorks = false;
    sinks.persist = (ops) => {
      if (!storageWorks) return Promise.reject(new DOMException("quota", "QuotaExceededError"));
      log.push(`persist ${ops.length}`);
      return Promise.resolve();
    };
    const stopped = vi.fn();
    const applier = new EffectApplier(sinks, stopped);

    await expect(applier.apply(sendAndStore)).rejects.toThrow(StorageFailedError);
    expect(applier.stopped).toBe(true);
    expect(stopped).toHaveBeenCalledTimes(1);

    // Storage working again does not matter: the core is past what was stored.
    storageWorks = true;
    await expect(applier.apply(sendAndStore)).rejects.toThrow(StorageFailedError);
    expect(log).toEqual([]);
    expect(stopped).toHaveBeenCalledTimes(1);
  });

  it("keeps going after a failure that is not storage", async () => {
    const { log, sinks } = recorder();
    sinks.openSink = () => Promise.reject(new Error("no room in OPFS"));
    const stopped = vi.fn();
    const applier = new EffectApplier(sinks, stopped);
    await expect(applier.apply([{ kind: "open_sink", file_id: "f", len: 1, sink: "download" }])).rejects.toThrow(
      "no room in OPFS",
    );
    await applier.apply(sendAndStore);
    expect(applier.stopped).toBe(false);
    expect(stopped).not.toHaveBeenCalled();
    expect(log).toEqual(["persist put:outbox", "transmit 9", "event message"]);
  });

  it("stops for good when what came before a failed sink cannot be stored", async () => {
    const { sinks } = recorder();
    sinks.persist = () => Promise.reject(new DOMException("quota", "QuotaExceededError"));
    sinks.closeSink = () => Promise.reject(new Error("gone"));
    const stopped = vi.fn();
    const applier = new EffectApplier(sinks, stopped);
    await expect(
      applier.apply([
        { kind: "put", table: "transfers", key: bytes(1), value: bytes(2) },
        { kind: "close_sink", file_id: "f", complete: true },
      ]),
    ).rejects.toThrow(StorageFailedError);
    expect(applier.stopped).toBe(true);
    expect(stopped).toHaveBeenCalledTimes(1);
  });

  it("stops for good on an effect it does not know", async () => {
    const { sinks } = recorder();
    const stopped = vi.fn();
    const applier = new EffectApplier(sinks, stopped);
    await expect(applier.apply([{ kind: "launch" } as unknown as Effect])).rejects.toThrow(StorageFailedError);
    expect(applier.stopped).toBe(true);
    expect(stopped).toHaveBeenCalledTimes(1);
  });
});
