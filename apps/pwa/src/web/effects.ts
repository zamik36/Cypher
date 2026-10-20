/** Core effects as `cypher-wasm` emits them and the order they are applied in. */
import type { Effect, Op, ReadChunk, Reply } from "../wasm/cypher_wasm.js";

/** Where a transfer's bytes are kept. */
export type Sink = Extract<Effect, { kind: "open_sink" }>["sink"];

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
  /** The sink calls report a disk failure to the core rather than throw. */
  openSink(fileId: string, len: number, sink: Sink): Promise<void>;
  writeChunk(fileId: string, offset: number, data: Uint8Array): void;
  closeSink(fileId: string, complete: boolean): Promise<void>;
  /** Writes the message history archive for a newly linked device. */
  buildHistory(fileId: string, partial: string[]): void;
  /** Takes in the rows of a received history archive. */
  importHistory(fileId: string): void;
  disconnect(reconnect: boolean): void;
}

/**
 * A storage batch could not be made durable, or an effect could not be
 * applied at all; nothing after it was applied.
 */
export class StorageFailedError extends Error {
  constructor(readonly reason: unknown) {
    super(`storage failed: ${reason instanceof Error ? reason.message : String(reason)}`);
    this.name = "StorageFailedError";
  }
}

/**
 * An effect this worker does not know may carry state the core counts on,
 * so it stops the client like a failed write.
 */
function unknownEffect(effect: never): StorageFailedError {
  return new StorageFailedError(new Error(`unknown effect ${JSON.stringify(effect)}`));
}

/**
 * Applies effects strictly in order. Storage operations are batched, and a
 * batch is durable before anything that follows it leaves the device —
 * the same guarantee the native driver gives. A batch that fails rejects
 * with {@link StorageFailedError} and stops the rest. Any other failure
 * stops the rest too, but what was staged before it is still stored.
 */
export async function applyEffects(effects: Effect[], sinks: EffectSinks): Promise<void> {
  let batch: Op[] = [];
  const flush = async () => {
    if (batch.length === 0) return;
    const ops = batch;
    batch = [];
    try {
      await sinks.persist(ops);
    } catch (e) {
      throw new StorageFailedError(e);
    }
  };
  try {
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
          await sinks.openSink(e.file_id, e.len, e.sink);
          break;
        case "write_chunk":
          sinks.writeChunk(e.file_id, e.offset, e.data);
          break;
        case "close_sink":
          await sinks.closeSink(e.file_id, e.complete);
          break;
        case "build_history":
          await flush();
          sinks.buildHistory(e.file_id, e.partial);
          break;
        case "import_history":
          await flush();
          sinks.importHistory(e.file_id);
          break;
        case "disconnect":
          sinks.disconnect(e.reconnect);
          break;
        default:
          await flush();
          throw unknownEffect(e);
      }
    }
  } catch (e) {
    // The core is already past the operations staged before the failure.
    if (!(e instanceof StorageFailedError)) await flush();
    throw e;
  }
  await flush();
}

/**
 * Applies the effects of one core, and stops for good once a storage batch
 * failed: the core has then moved past what is stored, so anything computed
 * from it could reuse ratchet keys after a restart. `onStorageFailed` runs
 * once, when that happens; every later batch is refused without being looked
 * at, and the core must not be fed any more ({@link stopped}).
 */
export class EffectApplier {
  private failed = false;

  constructor(
    private readonly sinks: EffectSinks,
    private readonly onStorageFailed: () => void,
  ) {}

  /** True once a storage batch failed. */
  get stopped(): boolean {
    return this.failed;
  }

  async apply(effects: Effect[]): Promise<void> {
    if (this.failed) throw new StorageFailedError("stopped after an earlier failure");
    try {
      await applyEffects(effects, this.sinks);
    } catch (e) {
      if (e instanceof StorageFailedError) {
        this.failed = true;
        this.onStorageFailed();
      }
      throw e;
    }
  }
}
