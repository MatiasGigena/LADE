import { MAX_FRAME, WireError } from "../json.ts";
import type { ServiceToWorker, WorkerToService } from "../dto.ts";
import { BUNDLE, isServiceToWorker, isWorkerToService, VERSION } from "../schema.ts";
import { decodeWorkerFrame } from "./contract.ts";

export type StartBody = Extract<ServiceToWorker, { kind: "worker.start" }>;

export type DeliverBody = Extract<ServiceToWorker, { kind: "input.deliver" }>;

export type CommandBody = Extract<ServiceToWorker, { kind: "session.command" }>;

export type RespondBody = Extract<ServiceToWorker, { kind: "ui.respond" }>;

export type ActivityItemBody = Extract<WorkerToService, { kind: "activity.item" }>;

export type ResultBody = Extract<WorkerToService, { kind: "session.result" }>;

export type LoadedResource = Extract<WorkerToService, { kind: "resources.loaded" }>["resources"][number];

export type TurnState = Extract<WorkerToService, { kind: "turn.state" }>["state"];

export function parseInbound(bytes: Uint8Array): ServiceToWorker {
  const { body } = decodeWorkerFrame(bytes);

  if (!isServiceToWorker(body)) throw new WireError("invalid_schema");

  return body;
}

export function encodeOutbound(body: WorkerToService): Buffer {
  const text = JSON.stringify({ protocol: VERSION, bundle: BUNDLE, body });

  if (Buffer.byteLength(text) > MAX_FRAME) throw new WireError("frame_size");
  const bytes = Buffer.from(text);

  if (!isWorkerToService(decodeWorkerFrame(bytes).body)) throw new WireError("invalid_schema");

  return Buffer.concat([bytes, Buffer.from("\n")]);
}

export class LineDecoder<Frame> {
  private readonly decode: (bytes: Uint8Array) => Frame;
  private body = Buffer.alloc(0);
  private size = 0;
  private failed = false;
  constructor(decode: (bytes: Uint8Array) => Frame) { this.decode = decode; }

  push(input: Buffer, emit: (frame: Frame) => void): void {
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
          const frame = this.decode(this.body.subarray(0, this.size));
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
