import type { Envelope, WorkerBody, WorkerEnvelope } from "../../src/dto.ts";
import { MAX_FRAME, WireError } from "../../src/json.ts";
import { BUNDLE, decode, decodeWorkerFrame, validate, validateWorker, VERSION } from "./protocol.ts";
import { LineDecoder } from "../../src/worker/wire.ts";

export type Framing = "client" | "worker";

export interface FrameDecoder<Frame> {
  push(input: Buffer, emit: (frame: Frame) => void): void;
  finish(): void;
}

class LengthDecoder<Frame> implements FrameDecoder<Frame> {
  private readonly decodeBody: (bytes: Uint8Array) => Frame;
  private readonly header = Buffer.alloc(4);
  private headerSize = 0;
  private body = Buffer.alloc(0);
  private size = 0;
  private expected: number | undefined;
  private failed = false;
  constructor(decodeBody: (bytes: Uint8Array) => Frame) { this.decodeBody = decodeBody; }

  push(input: Buffer, emit: (frame: Frame) => void): void {
    if (this.failed) throw new WireError("closed");

    try {
      let pos = 0;

      while (pos < input.length) {
        if (this.expected === undefined) {
          const n = Math.min(4 - this.headerSize, input.length - pos);
          input.copy(this.header, this.headerSize, pos, pos + n);
          this.headerSize += n; pos += n;

          if (this.headerSize !== 4) continue;
          const size = this.header.readUInt32BE();
          this.headerSize = 0;

          if (size === 0 || size > MAX_FRAME) throw new WireError("frame_size");
          this.expected = size;
          this.body = Buffer.alloc(size);
        }

        const n = Math.min(this.expected - this.size, input.length - pos);
        input.copy(this.body, this.size, pos, pos + n);
        this.size += n; pos += n;

        if (this.size === this.expected) {
          const frame = this.decodeBody(this.body);
          this.size = 0; this.expected = undefined; this.body = Buffer.alloc(0);
          emit(frame);
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
    const partial = this.size !== 0 || this.headerSize !== 0 || this.expected !== undefined;
    this.body = Buffer.alloc(0);

    if (partial) throw new WireError("truncated_frame");
  }
}

export function decoder<Frame>(framing: Framing, decodeBody: (bytes: Uint8Array) => Frame): FrameDecoder<Frame> {
  return framing === "worker" ? new LineDecoder(decodeBody) : new LengthDecoder(decodeBody);
}

function framed(body: Buffer, framing: Framing): Buffer {
  if (framing === "worker") return Buffer.concat([body, Buffer.from("\n")]);
  const header = Buffer.alloc(4); header.writeUInt32BE(body.length);

  return Buffer.concat([header, body]);
}

export function encode(frame: Envelope, framing: Framing): Buffer {
  validate(frame);
  const body = Buffer.from(JSON.stringify(frame));
  decode(body);

  return framed(body, framing);
}

export function encodeWorker(frame: WorkerEnvelope, framing: Framing): Buffer {
  validateWorker(frame);
  const body = Buffer.from(JSON.stringify(frame));
  decodeWorkerFrame(body);

  return framed(body, framing);
}

export function encodeWorkerBody(body: WorkerBody): Buffer {
  return encodeWorker({ protocol: VERSION, bundle: BUNDLE, body }, "worker");
}
