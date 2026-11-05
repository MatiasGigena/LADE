import { once } from "node:events";
import type { EventEmitter } from "node:events";
import type { Readable } from "node:stream";
import type { ServiceToWorker, WorkerToService } from "../dto.ts";
import { WireError } from "../json.ts";
import type { WireCode } from "../json.ts";
import { encodeOutbound, LineDecoder, parseInbound } from "./wire.ts";

const DELTA_BUDGET = 4 * 1024 * 1024;

const MAX_COALESCED_DELTA = 64 * 1024;

/** Estimated bytes of a queued frame's envelope and fixed fields, beyond its text. */
const FRAME_OVERHEAD = 256;

const DELTA_OVERHEAD = 128;

/** A service frame the decoder rejected: a protocol violation, unlike a `WireError` raised for the worker's own frames. */
export class InboundError extends Error {
  readonly name = "InboundError";
  readonly code: WireCode;
  constructor(code: WireCode) { super(`rejected inbound frame (${code})`); this.code = code; }
}

/** Runs one decoder step; a `WireError` it throws becomes an `InboundError`, a protocol violation of the service's frame. */
function asInbound(step: () => void): void {
  try {
    step();
  } catch (error) {
    throw error instanceof WireError ? new InboundError(error.code) : error;
  }
}

function weight(body: WorkerToService): number {
  switch (body.kind) {
    case "activity.item": return Buffer.byteLength(body.text) + FRAME_OVERHEAD;
    case "activity.delta": return Buffer.byteLength(body.append_text) + DELTA_OVERHEAD;
    default: return FRAME_OVERHEAD;
  }
}

export class StdioChannel {
  private readonly input: Readable;
  private readonly write: (frame: Buffer) => boolean;
  private readonly drains: EventEmitter;
  private readonly queue: WorkerToService[] = [];
  private queued = 0;
  private flushing: Promise<void> | undefined;
  constructor(input: Readable, write: (frame: Buffer) => boolean, drains: EventEmitter) {
    this.input = input; this.write = write; this.drains = drains;
  }

  /**
   * Queues a frame and resolves once the queue is written. Every frame is kept except `activity.delta`, which is
   * best-effort: consecutive deltas of one item coalesce, and above DELTA_BUDGET of queued output a delta that cannot
   * coalesce is dropped. Nothing is lost by the drop, because the `activity.item` that ends every streamed block
   * carries its complete bounded text and replaces what was streamed.
   */
  send(body: WorkerToService): Promise<void> {
    const last = this.queue.at(-1);

    if (body.kind === "activity.delta") {
      if (last?.kind === "activity.delta" && last.item_id === body.item_id && Buffer.byteLength(last.append_text) + Buffer.byteLength(body.append_text) <= MAX_COALESCED_DELTA) {
        last.append_text += body.append_text; this.queued += Buffer.byteLength(body.append_text);

        return this.flush();
      }

      if (this.queued > DELTA_BUDGET) return this.flush();
    }

    this.queue.push(body); this.queued += weight(body);

    return this.flush();
  }

  flush(): Promise<void> {
    this.flushing ??= this.drain().finally(() => { this.flushing = undefined; });

    return this.flushing.then(() => this.queue.length > 0 ? this.flush() : undefined);
  }

  private async drain(): Promise<void> {
    for (let body = this.queue.shift(); body !== undefined; body = this.queue.shift()) {
      this.queued -= weight(body);

      if (!this.write(encodeOutbound(body))) await once(this.drains, "drain");
    }

    this.queued = 0;
  }

  async *incoming(): AsyncGenerator<ServiceToWorker> {
    const decoder = new LineDecoder(parseInbound);
    const frames: ServiceToWorker[] = [];

    for await (const chunk of this.input) {
      asInbound(() => decoder.push(Buffer.from(chunk), frame => { frames.push(frame); }));

      for (let frame = frames.shift(); frame !== undefined; frame = frames.shift()) yield frame;
    }

    asInbound(() => decoder.finish());
  }
}
