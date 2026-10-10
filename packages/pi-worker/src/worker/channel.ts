import { once } from "node:events";
import type { EventEmitter } from "node:events";
import type { Readable } from "node:stream";
import { encodeOutbound, LineDecoder } from "./wire.ts";
import type { InboundBody, OutboundBody, WorkerChannel } from "./wire.ts";

/** Above this many queued bytes, streaming deltas are dropped; the final activity.item still lands. */
export const DELTA_BUDGET = 4 * 1024 * 1024;

const MAX_COALESCED_DELTA = 64 * 1024;

function weight(body: OutboundBody): number {
  switch (body.kind) {
    case "activity.item": return Buffer.byteLength(body.text) + 256;
    case "activity.delta": return Buffer.byteLength(body.append_text) + 128;
    default: return 256;
  }
}

/**
 * JSONL over inherited pipes. Inbound frames are yielded one at a time, so stdin is only read while
 * the worker is ready for the next frame. Outbound frames are written strictly in order and every
 * write that reports a full pipe waits for "drain" before the next one.
 */
export class StdioChannel implements WorkerChannel {
  private readonly input: Readable;
  private readonly write: (frame: Buffer) => boolean;
  private readonly drains: EventEmitter;
  private readonly queue: OutboundBody[] = [];
  private queued = 0;
  private flushing: Promise<void> | undefined;
  constructor(input: Readable, write: (frame: Buffer) => boolean, drains: EventEmitter) {
    this.input = input; this.write = write; this.drains = drains;
  }

  send(body: OutboundBody): Promise<void> {
    const last = this.queue.at(-1);

    if (body.kind === "activity.delta") {
      if (last?.kind === "activity.delta" && last.item_id === body.item_id && Buffer.byteLength(last.append_text) < MAX_COALESCED_DELTA) {
        last.append_text += body.append_text; this.queued += weight(body);

        return this.flush();
      }

      if (this.queued > DELTA_BUDGET) return this.flush();
    }

    this.queue.push(body); this.queued += weight(body);

    return this.flush();
  }

  /** Resolves once every frame queued so far has been handed to the pipe. */
  flush(): Promise<void> {
    this.flushing ??= this.drain().finally(() => { this.flushing = undefined; });

    return this.flushing;
  }

  private async drain(): Promise<void> {
    for (let body = this.queue.shift(); body !== undefined; body = this.queue.shift()) {
      this.queued -= weight(body);

      if (!this.write(encodeOutbound(body))) await once(this.drains, "drain");
    }

    this.queued = 0;
  }

  async *incoming(): AsyncGenerator<InboundBody> {
    const decoder = new LineDecoder();
    const frames: InboundBody[] = [];

    for await (const chunk of this.input) {
      decoder.push(Buffer.from(chunk), frame => { frames.push(frame); });

      for (let frame = frames.shift(); frame !== undefined; frame = frames.shift()) yield frame;
    }

    decoder.finish();
  }
}
