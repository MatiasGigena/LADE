import type { Envelope } from "./dto.ts";
import { MAX_FRAME, WireError } from "./json.ts";
import { decode, validate } from "./protocol.ts";

export type Framing = "client" | "worker";

export class Decoder {
  private readonly framing: Framing;
  private readonly header = Buffer.alloc(4);
  private headerSize = 0;
  private body = Buffer.alloc(0);
  private size = 0;
  private expected: number | undefined;
  private failed = false;
  constructor(framing: Framing) { this.framing = framing; }

  push(input: Buffer, emit: (frame: Envelope) => void): void {
    if (this.failed) throw new WireError("closed");

    try {
      let pos = 0;

      while (pos < input.length) {
        if (this.framing === "client") {
          if (this.expected === undefined) {
            const n = Math.min(4 - this.headerSize, input.length - pos);
            input.copy(this.header, this.headerSize, pos, pos + n);
            this.headerSize += n; pos += n;

            if (this.headerSize !== 4) continue;
            const size = this.header.readUInt32BE();
            this.headerSize = 0;

            if (size === 0 || size > MAX_FRAME) throw new WireError("frame_size");
            this.expected = size;
            this.body = Buffer.alloc(size); // Only after validating the declared length.
          }

          const n = Math.min(this.expected - this.size, input.length - pos);
          input.copy(this.body, this.size, pos, pos + n);
          this.size += n; pos += n;

          if (this.size === this.expected) {
            const frame = decode(this.body);
            this.size = 0; this.expected = undefined; this.body = Buffer.alloc(0);
            emit(frame);
          }
        } else {
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
            const frame = decode(this.body.subarray(0, this.size));
            this.size = 0; pos++; emit(frame);
          }
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

export function encode(frame: Envelope, framing: Framing): Buffer {
  validate(frame); // Closed, bounded fields before JSON.stringify allocates.
  const body = Buffer.from(JSON.stringify(frame));
  decode(body);

  if (framing === "worker") return Buffer.concat([body, Buffer.from("\n")]);
  const header = Buffer.alloc(4); header.writeUInt32BE(body.length);

  return Buffer.concat([header, body]);
}
