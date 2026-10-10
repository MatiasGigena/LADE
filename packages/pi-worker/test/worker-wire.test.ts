import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { Readable } from "node:stream";
import { test } from "node:test";
import { boundText, MAX_ITEM_BYTES, safeMessage } from "../src/worker/activity.ts";
import { StdioChannel } from "../src/worker/channel.ts";
import { encodeOutbound, LineDecoder, parseInbound } from "../src/worker/wire.ts";
import type { InboundBody } from "../src/worker/wire.ts";
import type { JsonObject } from "../src/json.ts";

const ID = "00000000-0000-4000-8000-000000000001";

function frame(body: JsonObject, bundle = "0.2.0"): Buffer {
  return Buffer.from(JSON.stringify({ protocol: 1, bundle, body }));
}

test("inbound bodies are closed and exactly typed", () => {
  assert.deepEqual(parseInbound(frame({ kind: "input.deliver", input_id: ID, mode: "steer", text: "x" })), { kind: "input.deliver", input_id: ID, mode: "steer", text: "x" });
  assert.deepEqual(parseInbound(frame({ kind: "session.abort" })), { kind: "session.abort" });
  assert.throws(() => parseInbound(frame({ kind: "session.abort", extra: true })), { code: "invalid_schema" });
  assert.throws(() => parseInbound(frame({ kind: "input.deliver", input_id: "not-a-uuid", mode: "prompt", text: "x" })), { code: "invalid_schema" });
  assert.throws(() => parseInbound(frame({ kind: "input.deliver", input_id: ID, mode: "later", text: "x" })), { code: "invalid_schema" });
  assert.throws(() => parseInbound(frame({ kind: "worker.start", objective_id: ID, session_dir: "/s", cwd: "/c", model: { provider: "p" }, resume: false, system_note: "" })), { code: "invalid_schema" });
  assert.throws(() => parseInbound(frame({ kind: "worker.shutdown" }, "0.1.0-imp001")), { code: "version_mismatch" });
  assert.throws(() => parseInbound(Buffer.from('{"protocol":1,"protocol":1}')), { code: "duplicate_key" });
});

test("LF framing survives byte fragmentation and poisons after an error", () => {
  const wire = Buffer.concat([frame({ kind: "session.abort" }), Buffer.from("\n"), frame({ kind: "worker.shutdown" }), Buffer.from("\n")]);
  const decoder = new LineDecoder();
  const seen: InboundBody[] = [];

  for (let pos = 0; pos < wire.length; pos++) decoder.push(wire.subarray(pos, pos + 1), body => seen.push(body));
  decoder.finish();
  assert.deepEqual(seen.map(body => body.kind), ["session.abort", "worker.shutdown"]);

  const broken = new LineDecoder();
  assert.throws(() => broken.push(Buffer.from("{oops}\n"), () => {}), { code: "invalid_json" });
  assert.throws(() => broken.push(Buffer.concat([frame({ kind: "session.abort" }), Buffer.from("\n")]), () => {}), { code: "closed" });

  const partial = new LineDecoder();
  partial.push(frame({ kind: "session.abort" }), () => {});
  assert.throws(() => partial.finish(), { code: "truncated_frame" });
});

test("outbound frames are bounded before they are written", () => {
  assert.match(encodeOutbound({ kind: "turn.state", state: "idle" }).toString(), /^\{"protocol":1,"bundle":"0\.2\.0","body":\{"kind":"turn.state","state":"idle"\}\}\n$/);
  assert.throws(() => encodeOutbound({ kind: "worker.error", code: "internal", message: "m".repeat(513) }), { code: "invalid_schema" });
  assert.throws(() => encodeOutbound({ kind: "activity.delta", item_id: ID, append_text: "a".repeat(300 * 1024) }), { code: "limit_reached" });
});

test("item text is bounded by bytes, UTF-8 boundaries and the encoded frame size", () => {
  const wide = boundText("é".repeat(MAX_ITEM_BYTES));
  assert.equal(wide.truncated, true);
  assert.ok(Buffer.byteLength(wide.text) <= MAX_ITEM_BYTES);
  assert.equal(wide.text.includes("�"), false);

  const control = boundText("\u0001".repeat(MAX_ITEM_BYTES));
  assert.equal(control.truncated, true);
  assert.ok(encodeOutbound({ kind: "activity.item", item_id: ID, role: "tool", title: "t", text: control.text, is_error: false, truncated: true }).length < 1024 * 1024);
  assert.equal(boundText("\uD800").text, "�", "lone surrogates are normalised before JSON encoding");
  assert.equal(safeMessage(`401: key sk-${"a".repeat(40)}\nrest`), "401: key [redacted] rest");
});

test("outbound writes wait for drain and coalesce deltas", async () => {
  const drains = new EventEmitter();
  const written: string[] = [];
  let full = true;

  const channel = new StdioChannel(Readable.from([]), bytes => { written.push(bytes.toString());

 return !full; }, drains);

  void channel.send({ kind: "turn.state", state: "running" });
  void channel.send({ kind: "activity.delta", item_id: ID, append_text: "a" });
  void channel.send({ kind: "activity.delta", item_id: ID, append_text: "b" });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(written.length, 1, "the second frame waits for drain");
  full = false;
  drains.emit("drain");
  await channel.flush();
  assert.equal(written.length, 2);
  assert.match(written[1] ?? "", /"append_text":"ab"/);
});
