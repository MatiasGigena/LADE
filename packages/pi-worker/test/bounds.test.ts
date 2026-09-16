import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { Reassembler } from "../src/chunks.ts";
import { decode, MAX_CHUNK, MAX_TRANSFER, BUNDLE } from "../src/protocol.ts";
import { Decoder, encode } from "../src/framing.ts";
import { RequestHistory } from "../src/history.ts";
import { WireError, MAX_FRAME } from "../src/json.ts";
import type { Counter, Scope } from "../src/dto.ts";

function id(n: number): string { return `00000000-0000-4000-8000-${n.toString(16).padStart(12, "0")}`; }
const scope = { workspace_id: id(1), objective_id: id(2), session_id: id(3), association_revision: "1" };
function message(body: unknown) { return decode(Buffer.from(JSON.stringify({ protocol: 1, bundle: BUNDLE, body }))); }
function reassembler(): Reassembler {
  return new Reassembler(scope as Scope, "1" as Counter);
}

test("logical content larger than a frame survives at the reassembly limit", () => {
  const bytes = Buffer.alloc(MAX_TRANSFER, 97);
  const chunks = reassembler();
  chunks.accept(message({ kind: "chunk.start", transfer_id: id(4), owner: scope, execution_generation: "1", total_bytes: String(MAX_TRANSFER), sha256: createHash("sha256").update(bytes).digest("hex") }).body);
  for (let index = 0; index < MAX_TRANSFER / MAX_CHUNK; index++) {
    const data = message({ kind: "chunk.data", transfer_id: id(4), index: String(index), hex: "61".repeat(MAX_CHUNK) });
    const decoder = new Decoder("worker");
    decoder.push(encode(data, "worker"), envelope => assert.equal(chunks.accept(envelope.body), undefined));
    decoder.finish();
  }
  assert.deepEqual(chunks.accept(message({ kind: "chunk.end", transfer_id: id(4) }).body), bytes);
});

test("cancellation releases capacity and disconnect revokes reassembly", () => {
  const chunks = reassembler();
  for (let n = 0; n < 20; n++) {
    chunks.accept(message({ kind: "chunk.start", transfer_id: id(4 + n), owner: scope, execution_generation: "1", total_bytes: "1", sha256: "0".repeat(64) }).body);
    chunks.accept(message({ kind: "chunk.cancel", transfer_id: id(4 + n) }).body);
  }
  chunks.close();
  assert.throws(() => chunks.accept(message({ kind: "chunk.end", transfer_id: id(4) }).body), { code: "closed" });
});

test("correlation overflow never evicts the original outcome", () => {
  const history = new RequestHistory();
  const command = (n: number) => message({ kind: "command", request_id: id(1000 + n), operation_id: id(2000 + n), expected_service_epoch: id(6), scope, command: { name: "operation.get", payload: { operation_id: id(5) } } });
  for (let n = 0; n < 256; n++) history.observe(command(n).body, { status: "operation", state: "uncertain" });
  assert.throws(() => history.observe(command(256).body, { status: "operation", state: "completed" }), { code: "limit_reached" });
  assert.deepEqual(history.observe(command(0).body, { status: "operation", state: "completed" }), { status: "operation", state: "uncertain" });
});

test("downstream backpressure stops before later frames", () => {
  for (const framing of ["client", "worker"] as const) {
    const hello = encode(message({ kind: "hello" }), framing);
    const wire = Buffer.concat([hello, hello]);
    const decoder = new Decoder(framing);
    let calls = 0;
    assert.throws(() => decoder.push(wire, () => { calls++; throw new WireError("limit_reached"); }), { code: "limit_reached" });
    assert.equal(calls, 1);
    assert.throws(() => decoder.push(wire, () => {}), { code: "closed" });
  }
});

test("oversized outbound content rejects before serialization", () => {
  const data = message({ kind: "chunk.data", transfer_id: id(4), index: "0", hex: "61" });
  assert.equal(data.body.kind, "chunk.data");
  if (data.body.kind === "chunk.data") data.body.hex = "a".repeat(2 * MAX_FRAME);
  assert.throws(() => encode(data, "client"), { code: "invalid_schema" });
});
