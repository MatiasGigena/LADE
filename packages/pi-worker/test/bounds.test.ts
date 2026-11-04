import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { Reassembler } from "../src/chunks.ts";
import { decode, decodeWorkerFrame, isServiceToWorker, isWorkerToService, MAX_CHUNK, MAX_TRANSFER, BUNDLE, newId, parseId } from "../src/protocol.ts";
import { decoder, encode, encodeWorkerBody } from "../src/framing.ts";
import type { WorkerBody } from "../src/dto.ts";
import { RequestHistory } from "../src/history.ts";
import { boundedJson, WireError, MAX_FRAME, MAX_NODES } from "../src/json.ts";
import type { JsonObject, JsonValue } from "../src/json.ts";

function id(n: number): string { return `00000000-0000-4000-8000-${n.toString(16).padStart(12, "0")}`; }

const scope = { workspace_id: id(1), objective_id: id(2), session_id: id(3), association_revision: "1" };

function message(body: JsonValue) { return decode(Buffer.from(JSON.stringify({ protocol: 1, bundle: BUNDLE, body }))); }

function reassembler(): Reassembler {
  const context = message({
    kind: "chunk.start",
    transfer_id: id(4),
    owner: scope,
    execution_generation: "1",
    total_bytes: "1",
    sha256: "0".repeat(64),
  });

  if (context.body.kind !== "chunk.start") throw new Error("invalid reassembly context");

  return new Reassembler(context.body.owner, context.body.execution_generation);
}

test("logical content larger than a frame survives at the reassembly limit", () => {
  const bytes = Buffer.alloc(MAX_TRANSFER, 97);
  const chunks = reassembler();
  chunks.accept(message({ kind: "chunk.start", transfer_id: id(4), owner: scope, execution_generation: "1", total_bytes: String(MAX_TRANSFER), sha256: createHash("sha256").update(bytes).digest("hex") }).body);

  for (let index = 0; index < MAX_TRANSFER / MAX_CHUNK; index++) {
    const data = message({ kind: "chunk.data", transfer_id: id(4), index: String(index), hex: "61".repeat(MAX_CHUNK) });
    const lines = decoder("worker", decode);
    lines.push(encode(data, "worker"), envelope => assert.equal(chunks.accept(envelope.body), undefined));
    lines.finish();
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
  const command = (n: number) => message({ kind: "request", request_id: id(1000 + n), operation_id: id(2000 + n), name: "draft.delete", payload: { draft_id: id(5), expected_revision: "1" } });
  const original = { status: "rejected", code: "interrupted", action: "query_operation", detail: "" } as const;
  const later = { status: "rejected", code: "conflict", action: "refresh", detail: "" } as const;

  for (let n = 0; n < 256; n++) history.observe(command(n).body, original);
  assert.throws(() => history.observe(command(256).body, later), { code: "limit_reached" });
  assert.deepEqual(history.observe(command(0).body, later), original);
});

test("downstream backpressure stops before later frames", () => {
  for (const framing of ["client", "worker"] as const) {
    const hello = encode(message({ kind: "hello" }), framing);
    const wire = Buffer.concat([hello, hello]);
    const frames = decoder(framing, decode);
    let calls = 0;
    assert.throws(() => frames.push(wire, () => { calls++; throw new WireError("limit_reached"); }), { code: "limit_reached" });
    assert.equal(calls, 1);
    assert.throws(() => frames.push(wire, () => {}), { code: "closed" });
  }
});

test("oversized outbound content rejects before serialization", () => {
  const data = message({ kind: "chunk.data", transfer_id: id(4), index: "0", hex: "61" });
  assert.equal(data.body.kind, "chunk.data");

  if (data.body.kind === "chunk.data") data.body.hex = "a".repeat(2 * MAX_FRAME);
  assert.throws(() => encode(data, "client"), { code: "invalid_schema" });
});

test("worker API encodes JSONL lines and decodes them one at a time", () => {
  const bodies: WorkerBody[] = [
    { kind: "worker.ready", pi_session_id: "pi-1", session_file: "/session/a.jsonl" },
    { kind: "activity.item", item_id: newId(), role: "assistant", title: "Reply", text: "a\u2028b\nc", is_error: false, truncated: false },
    { kind: "turn.state", state: "idle" },
  ];

  const wire = Buffer.concat(bodies.map(encodeWorkerBody));
  assert.equal(wire.toString().split("\n").length, bodies.length + 1);
  const lines = decoder("worker", decodeWorkerFrame);
  const received: WorkerBody[] = [];

  for (const byte of wire) lines.push(Buffer.from([byte]), frame => received.push(frame.body));
  lines.finish();
  assert.deepEqual(JSON.parse(JSON.stringify(received)), bodies);
  assert.ok(received.every(isWorkerToService));
});

test("worker API distinguishes directions and rejects client bodies", () => {
  const start = decodeWorkerFrame(encodeWorkerBody({ kind: "session.abort" }).subarray(0, -1));
  assert.ok(isServiceToWorker(start.body));
  assert.equal(isWorkerToService(start.body), false);
  assert.throws(() => decodeWorkerFrame(Buffer.from(JSON.stringify({ protocol: 1, bundle: BUNDLE, body: { kind: "hello" } }))), { code: "invalid_schema" });
  assert.throws(() => decodeWorkerFrame(Buffer.from(JSON.stringify({ protocol: 1, bundle: "0.1.0-imp001", body: { kind: "session.abort" } }))), { code: "version_mismatch" });
  assert.throws(() => encodeWorkerBody({ kind: "worker.error", code: "internal", message: "x".repeat(513) }), { code: "invalid_schema" });
  assert.throws(() => parseId("ABCDEF00-0000-4000-8000-000000000000"), { code: "invalid_schema" });
  assert.match(newId(), /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
});

test("a trailing or dangling array comma is invalid_json even when the node budget is spent", () => {
  const inner = `[${Array.from({ length: 380 }, () => "0").join(",")}]`;
  const body = Array.from({ length: 43 }, () => inner).join(",");
  assert.equal(1 + 43 * 381, MAX_NODES);
  assert.doesNotThrow(() => boundedJson(Buffer.from(`[${body}]`)));
  assert.throws(() => boundedJson(Buffer.from(`[${body},]`)), { code: "invalid_json" });
  assert.throws(() => boundedJson(Buffer.from(`[${body}, ]`)), { code: "invalid_json" });
  assert.throws(() => boundedJson(Buffer.from(`[${body},`)), { code: "invalid_json" });
  assert.throws(() => boundedJson(Buffer.from(`[${body},0]`)), { code: "limit_reached" });
});

test("draft and input pages round-trip at the exact frame bound and reject one byte over", () => {
  for (const name of ["drafts.list", "inputs.list"] as const) {
    const draft = name === "drafts.list";
    const field = draft ? "body" : "text_preview";

    const records: JsonObject[] = Array.from({ length: draft ? 2 : 200 }, (_, n) => draft ? {
      draft_id: id(1000 + n), objective_id: id(2), destination_session_id: id(3), body: "", revision: "1", state: "unsent", stale: false,
      reference: { artifact_digest: "a".repeat(64), path: { b64: "YQ==", display: "a" }, version: { kind: "working", capture_id: id(4) }, side: "new", byte_start: "0", byte_end: "1", selected_text: "a" },
    } : {
      input_id: id(1000 + n), objective_id: id(2), operation_id: id(3), mode: "prompt", text_preview: "", accepted_at: "2026-10-10T12:34:56.789Z", delivery: "queued",
    });

    const data: JsonObject = draft ? { drafts: records, next_after: { created_at: "2026-10-10T12:34:56.789Z", draft_id: id(1001) } } : { inputs: records, next_after: "200" };
    const page: JsonObject = { protocol: 1, bundle: BUNDLE, body: { kind: "reply", request_id: id(1), name, result: { status: "ok", data } } };
    let remaining = MAX_FRAME - Buffer.byteLength(JSON.stringify(page));

    for (const record of records) {
      const max = draft ? 256 * 1024 : 1024;
      const controls = Math.min(max, Math.floor(remaining / 6));
      const ascii = Math.min(max - controls, remaining - controls * 6);
      record[field] = "\u0001".repeat(controls) + "a".repeat(ascii);
      remaining -= controls * 6 + ascii;
    }

    assert.equal(remaining, 0);
    const bytes = Buffer.from(JSON.stringify(page));
    assert.equal(bytes.length, MAX_FRAME);
    const decoded = decode(bytes);
    assert.equal(encode(decoded, "client").length, MAX_FRAME + 4);
    assert.ok(decoded.body.kind === "reply" && "name" in decoded.body);

    if (decoded.body.name === "drafts.list") {
      assert.ok(decoded.body.result.status === "ok");
      const last = decoded.body.result.data.drafts.at(-1);
      assert.ok(last);
      last.body += "a";
    } else if (decoded.body.name === "inputs.list") {
      assert.ok(decoded.body.result.status === "ok");
      const last = decoded.body.result.data.inputs.at(-1);
      assert.ok(last);
      last.text_preview += "a";
    } else assert.fail("expected a record page");

    assert.throws(() => encode(decoded, "client"), { code: "frame_size" });
    assert.throws(() => decode(Buffer.from(JSON.stringify(decoded))), { code: "frame_size" });
  }
});
