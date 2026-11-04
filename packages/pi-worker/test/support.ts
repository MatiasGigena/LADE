import assert from "node:assert/strict";
import fixtures from "../../../contracts/v1/fixtures.json" with { type: "json" };
import { boundedJson, WireError } from "../src/json.ts";
import { BUNDLE, decode, decodeWorkerFrame } from "./reference/protocol.ts";
import { decoder, encode, encodeWorker } from "./reference/framing.ts";
import type { Framing } from "./reference/framing.ts";
import { Reassembler } from "./reference/chunks.ts";
import { RequestHistory } from "./reference/history.ts";
import type { ReplyResult } from "./reference/history.ts";
import type { Envelope, WorkerEnvelope } from "../src/dto.ts";

interface Source { value?: unknown; text?: string; hex?: string; depth?: number; array?: number; string?: number; nodes?: boolean; padding?: number; steps?: unknown[] }

interface Fixture { id: string; mode: string; source: Source; expected: string; framing?: string; contract?: string; owner?: unknown; generation?: string; output_hex?: string }

interface Codec<Frame> { decode: (bytes: Uint8Array) => Frame; encode: (frame: Frame, framing: Framing) => Buffer; valid: Buffer }

function validFrame(body: { kind: string }): Buffer { return Buffer.from(JSON.stringify({ protocol: 1, bundle: BUNDLE, body })); }

const clientCodec: Codec<Envelope> = { decode, encode, valid: validFrame({ kind: "hello" }) };

const workerCodec: Codec<WorkerEnvelope> = { decode: decodeWorkerFrame, encode: encodeWorker, valid: validFrame({ kind: "session.abort" }) };

export const corpus: Fixture[] = fixtures;

export function source(s: Source): Buffer {
  if (s.value !== undefined) return Buffer.from(JSON.stringify(s.value));

  if (s.text !== undefined) return Buffer.from(s.text);

  if (s.hex !== undefined) return Buffer.from(s.hex, "hex");

  if (s.depth !== undefined) return Buffer.from("[".repeat(s.depth) + "0" + "]".repeat(s.depth));

  if (s.array !== undefined) return Buffer.from(JSON.stringify(Array(s.array).fill(0)));

  if (s.string !== undefined) return Buffer.from(JSON.stringify("a".repeat(s.string)));

  if (s.nodes) return Buffer.from(JSON.stringify(Array.from({ length: 16 }, () => Array(1024).fill(0))));

  if (s.padding !== undefined) return Buffer.from("{}" + " ".repeat(s.padding - 2));
  throw new Error("unknown fixture recipe");
}

function framed(bytes: Buffer, framing: Framing): Buffer {
  if (framing === "worker") return Buffer.concat([bytes, Buffer.from("\n")]);
  const head = Buffer.alloc(4); head.writeUInt32BE(bytes.length);

  return Buffer.concat([head, bytes]);
}

function outcome(action: () => void): string {
  try {
    action();

    return "ok";
  } catch (error) {
    if (error instanceof WireError) return error.code;
    throw error;
  }
}

function transportFraming(fixture: Fixture): Framing {
  if (fixture.framing === "client" || fixture.framing === "worker") return fixture.framing;
  throw new Error("missing fixture framing");
}

function reassemblyContext(fixture: Fixture) {
  const envelope = decode(Buffer.from(JSON.stringify({
    protocol: 1,
    bundle: BUNDLE,
    body: {
      kind: "chunk.start",
      transfer_id: "00000000-0000-4000-8000-000000000000",
      owner: fixture.owner,
      execution_generation: fixture.generation,
      total_bytes: "1",
      sha256: "0".repeat(64),
    },
  })));

  if (envelope.body.kind !== "chunk.start") throw new Error("invalid reassembly context");

  return { owner: envelope.body.owner, generation: envelope.body.execution_generation };
}

function stream<Frame>(codec: Codec<Frame>, bytes: Buffer, framing: Framing, stride: number, expectedCount: number): string {
  const frames = decoder(framing, codec.decode);
  let count = 0;

  return outcome(() => {
    for (let pos = 0; pos < bytes.length; pos += stride) {
      try {
        frames.push(bytes.subarray(pos, pos + stride), message => {
          count++;
          const other = decoder(framing, codec.decode);
          other.push(codec.encode(message, framing), decoded => assert.deepEqual(decoded, message));
          other.finish();
        });
      } catch (error) {
        assert.ok(error instanceof WireError, "invalid bytes must produce a protocol rejection");
        assert.equal(count, 0, "invalid input must not emit a command");
        const valid = framed(codec.valid, framing);
        assert.equal(outcome(() => frames.push(valid, () => { count++; })), "closed");
        assert.equal(outcome(() => frames.finish()), "closed");
        assert.equal(count, 0, "a failed connection must not deliver later valid messages");
        throw error;
      }
    }

    frames.finish();
    assert.equal(count, expectedCount);
  });
}

function frameCase<Frame>(codec: Codec<Frame>, bytes: Buffer): string {
  let previous = outcome(() => assert.deepEqual(codec.decode(bytes), boundedJson(bytes)));

  for (const framing of ["client", "worker"] as const) {
    const wire = Buffer.concat([framed(bytes, framing), framed(bytes, framing), framed(codec.valid, framing)]);

    for (const stride of [1, 2, 3, 7, 65537, wire.length]) {
      const result = stream(codec, wire, framing, stride, 3);
      assert.equal(result, previous);
      previous = result;
    }
  }

  return previous;
}

function transportCase<Frame>(codec: Codec<Frame>, bytes: Buffer, framing: Framing): string {
  let previous: string | undefined;

  for (const stride of [1, 3, 65537, bytes.length]) {
    const result = stream(codec, bytes, framing, stride, 0);

    if (previous !== undefined) assert.equal(result, previous);
    previous = result;
  }

  return previous ?? "missing_result";
}

function rejected(code: "interrupted" | "durability_failure"): ReplyResult {
  return { status: "rejected", code, action: "query_operation", detail: "fixture" };
}

export function run(c: Fixture): string {
  return outcome(() => {
    switch (c.mode) {
      case "json":
        boundedJson(source(c.source));

        return;
      case "frame": {
        const result = c.contract === "worker" ? frameCase(workerCodec, source(c.source)) : frameCase(clientCodec, source(c.source));

        if (result !== "ok") throw new WireError(result);

        return;
      }

      case "transport": {
        const result = c.contract === "worker" ? transportCase(workerCodec, source(c.source), transportFraming(c)) : transportCase(clientCodec, source(c.source), transportFraming(c));

        if (result !== "ok") throw new WireError(result);

        return;
      }

      case "chunks": {
        const context = reassemblyContext(c);
        const chunks = new Reassembler(context.owner, context.generation);
        const output: Buffer[] = [];

        for (const step of c.source.steps ?? []) {
          const message = decode(Buffer.from(JSON.stringify(step)));

          try {
            const bytes = chunks.accept(message.body);

            if (bytes) output.push(bytes);
          } catch (error) {
            assert.throws(() => chunks.accept(message.body), { code: "closed" });
            throw error;
          }
        }

        assert.equal(Buffer.concat(output).toString("hex"), c.output_hex);
        chunks.close();

        return;
      }

      case "history": {
        const history = new RequestHistory();

        for (const [index, step] of (c.source.steps ?? []).entries()) {
          const message = decode(Buffer.from(JSON.stringify(step)));
          assert.deepEqual(history.observe(message.body, rejected(index === 0 ? "interrupted" : "durability_failure")), rejected("interrupted"));
        }

        return;
      }

      default: throw new Error("unknown fixture mode");
    }
  });
}
