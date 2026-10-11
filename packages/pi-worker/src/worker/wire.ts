// Worker wire shapes: the generated §4.2 DTOs (src/dto.ts, scripts/generate-dtos.py) validated
// against the checked-in closed worker schema (contracts/v1/worker-schema.json) in both directions.
import { boundedJson, MAX_FRAME, WireError } from "../json.ts";
import type { ServiceToWorker, WorkerBody, WorkerToService } from "../dto.ts";
import { BUNDLE, isServiceToWorker, isWorkerToService, VERSION } from "../schema.ts";
import { validateWorker } from "./contract.ts";

export type { ActivityRole, DeliveryMode, Model as ModelRef, SessionCommand, ThinkingLevel, TreeEntry, UiAnswer, UiPrompt, WorkerResources } from "../dto.ts";

export const WORKER_PROTOCOL = VERSION;

export const WORKER_BUNDLE = BUNDLE;

export type InboundBody = ServiceToWorker;

export type OutboundBody = WorkerToService;

export type StartBody = Extract<InboundBody, { kind: "worker.start" }>;

export type DeliverBody = Extract<InboundBody, { kind: "input.deliver" }>;

export type CommandBody = Extract<InboundBody, { kind: "session.command" }>;

export type RespondBody = Extract<InboundBody, { kind: "ui.respond" }>;

export type ActivityItemBody = Extract<OutboundBody, { kind: "activity.item" }>;

export type ResultBody = Extract<OutboundBody, { kind: "session.result" }>;

export type LoadedResource = Extract<OutboundBody, { kind: "resources.loaded" }>["resources"][number];

export type TurnState = Extract<OutboundBody, { kind: "turn.state" }>["state"];

/** Transport seam: worker logic never sees unvalidated frames. */
export interface WorkerChannel {
  send(body: OutboundBody): Promise<void>;
  incoming(): AsyncIterable<InboundBody>;
}

export const MAX_ERROR_MESSAGE = 512;

/** Bounded parse (IMP-001 JSON limits), exact versions, the closed worker schema and the service direction. */
export function parseInbound(bytes: Uint8Array): InboundBody {
  const value = boundedJson(bytes);
  validateWorker(value);
  const body: WorkerBody = value.body;

  if (!isServiceToWorker(body)) throw new WireError("invalid_schema");

  return body;
}

/** One LF-terminated worker frame; schema-checked and bounded before anything is written. */
export function encodeOutbound(body: OutboundBody): Buffer {
  const envelope = { protocol: WORKER_PROTOCOL, bundle: WORKER_BUNDLE, body };
  const text = JSON.stringify(envelope);

  if (Buffer.byteLength(text) > MAX_FRAME) throw new WireError("frame_size");
  const bytes = Buffer.from(text);
  // Re-decode exactly what leaves: a sender never emits what the peer must reject.
  const decoded = boundedJson(bytes);
  validateWorker(decoded);

  if (!isWorkerToService(decoded.body)) throw new WireError("invalid_schema");

  return Buffer.concat([bytes, Buffer.from("\n")]);
}

/**
 * Worker LF framing with the IMP-001 semantics of `Decoder("worker")` in ../framing.ts: split only
 * on byte LF, bound the pending line before allocating, poison the decoder after the first error.
 */
export class LineDecoder {
  private body = Buffer.alloc(0);
  private size = 0;
  private failed = false;

  push(input: Buffer, emit: (frame: InboundBody) => void): void {
    if (this.failed) throw new WireError("closed");

    try {
      let pos = 0;

      while (pos < input.length) {
        const end = input.indexOf(10, pos);
        const stop = end < 0 ? input.length : end;
        const n = stop - pos;

        if (this.size + n > MAX_FRAME) throw new WireError("frame_size");

        if (this.body.length < this.size + n) {
          const next = Buffer.alloc(Math.min(MAX_FRAME, Math.max(this.size + n, this.body.length * 2, 1024)));
          this.body.copy(next, 0, 0, this.size); this.body = next;
        }

        input.copy(this.body, this.size, pos, stop);
        this.size += n; pos = stop;

        if (end >= 0) {
          const frame = parseInbound(this.body.subarray(0, this.size));
          this.size = 0; pos++; emit(frame);
        }
      }
    } catch (error) {
      this.failed = true; this.body = Buffer.alloc(0); this.size = 0;
      throw error;
    }
  }

  finish(): void {
    if (this.failed) throw new WireError("closed");
    this.failed = true;
    const partial = this.size !== 0;
    this.body = Buffer.alloc(0);

    if (partial) throw new WireError("truncated_frame");
  }
}
