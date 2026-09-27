/** Core effects as `cypher-wasm` emits them and the order they are applied in. */
import type { Op } from "./idb";

export type Reply = { kind: "reply"; op: string; value: string; link?: string };
export type ReadChunk = {
  kind: "read_chunk";
  file_id: string;
  index: number;
  offset: number;
  len: number;
  headroom: number;
};

export type Effect =
  | { kind: "transmit" | "anonymous"; data: Uint8Array }
  | Op
  | { kind: "event"; channel: string; payload: unknown }
  | Reply
  | ReadChunk
  | { kind: "open_sink"; file_id: string; len: number; sealed: boolean }
  | { kind: "write_chunk"; file_id: string; offset: number; data: Uint8Array }
  | { kind: "close_sink"; file_id: string; complete: boolean }
  | { kind: "disconnect"; reconnect: boolean };

/** Where each kind of effect goes; the worker wires these to IndexedDB, sockets and OPFS. */
export interface EffectSinks {
  /** Durably applies one batch of storage operations. */
  persist(ops: Op[]): Promise<void>;
  transmit(data: Uint8Array): void;
  anonymous(data: Uint8Array): void;
  event(channel: string, payload: unknown): void;
  reply(reply: Reply): void;
  /** Starts an asynchronous read; its result is fed back to the core later. */
  readChunk(request: ReadChunk): void;
  openSink(fileId: string, len: number, sealed: boolean): Promise<void>;
  writeChunk(fileId: string, offset: number, data: Uint8Array): void;
  closeSink(fileId: string, complete: boolean): Promise<void>;
  disconnect(reconnect: boolean): void;
}

/**
 * Applies effects strictly in order. Storage operations are batched, and a
 * batch is durable before anything that follows it leaves the device —
 * the same guarantee the native driver gives.
 */
export async function applyEffects(effects: Effect[], sinks: EffectSinks): Promise<void> {
  let batch: Op[] = [];
  const flush = async () => {
    if (batch.length === 0) return;
    const ops = batch;
    batch = [];
    await sinks.persist(ops);
  };
  for (const e of effects) {
    switch (e.kind) {
      case "put":
      case "delete":
        batch.push(e);
        break;
      case "transmit":
        await flush();
        sinks.transmit(e.data);
        break;
      case "anonymous":
        await flush();
        sinks.anonymous(e.data);
        break;
      case "event":
        sinks.event(e.channel, e.payload);
        break;
      case "reply":
        sinks.reply(e);
        break;
      case "read_chunk":
        sinks.readChunk(e);
        break;
      case "open_sink":
        await sinks.openSink(e.file_id, e.len, e.sealed);
        break;
      case "write_chunk":
        sinks.writeChunk(e.file_id, e.offset, e.data);
        break;
      case "close_sink":
        await sinks.closeSink(e.file_id, e.complete);
        break;
      case "disconnect":
        sinks.disconnect(e.reconnect);
        break;
    }
  }
  await flush();
}
