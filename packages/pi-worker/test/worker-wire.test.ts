import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { Readable } from "node:stream";
import { test } from "node:test";
import type { AssistantMessage } from "../src/worker/pi-ai.ts";
import { ActivityMapper, boundText } from "../src/worker/activity.ts";
import { StdioChannel } from "../src/worker/channel.ts";
import { isInside } from "../src/worker/paths.ts";
import { safeMessage } from "../src/worker/text.ts";
import { MAX_TREE_ENTRIES, treeEntry, treePage } from "../src/worker/tree.ts";
import { UiBridge } from "../src/worker/ui.ts";
import { encodeOutbound, LineDecoder, parseInbound } from "../src/worker/wire.ts";
import type { ServiceToWorker, WorkerToService } from "../src/dto.ts";
import { MAX_FRAME, MAX_STRING } from "../src/json.ts";
import type { JsonObject } from "../src/json.ts";
import { MAX_ITEM_TEXT_BYTES, newId, parseId } from "../src/schema.ts";
import { decodeWorkerFrame } from "../src/worker/contract.ts";

const ID = parseId("00000000-0000-4000-8000-000000000001");

function frame(body: JsonObject, bundle = "0.2.0"): Buffer {
  return Buffer.from(JSON.stringify({ protocol: 1, bundle, body }));
}

test("inbound bodies are closed and exactly typed", () => {
  assert.deepEqual({ ...parseInbound(frame({ kind: "input.deliver", input_id: ID, mode: "steer", text: "x" })) }, { kind: "input.deliver", input_id: ID, mode: "steer", text: "x" });
  assert.deepEqual({ ...parseInbound(frame({ kind: "session.abort" })) }, { kind: "session.abort" });
  assert.throws(() => parseInbound(frame({ kind: "session.abort", extra: true })), { code: "invalid_schema" });
  assert.throws(() => parseInbound(frame({ kind: "input.deliver", input_id: "not-a-uuid", mode: "prompt", text: "x" })), { code: "invalid_schema" });
  assert.throws(() => parseInbound(frame({ kind: "input.deliver", input_id: ID, mode: "later", text: "x" })), { code: "invalid_schema" });
  assert.throws(() => parseInbound(frame({ kind: "worker.start", objective_id: ID, session_dir: "/s", cwd: "/c", model: { provider: "p" }, resume: false, system_note: "" })), { code: "invalid_schema" });
  assert.throws(() => parseInbound(frame({ kind: "worker.shutdown" }, "0.1.0-imp001")), { code: "version_mismatch" });
  assert.throws(() => parseInbound(Buffer.from('{"protocol":1,"protocol":1}')), { code: "duplicate_key" });
});

test("LF framing survives byte fragmentation and poisons after an error", () => {
  const wire = Buffer.concat([frame({ kind: "session.abort" }), Buffer.from("\n"), frame({ kind: "worker.shutdown" }), Buffer.from("\n")]);
  const decoder = new LineDecoder(parseInbound);
  const seen: ServiceToWorker[] = [];

  for (let pos = 0; pos < wire.length; pos++) decoder.push(wire.subarray(pos, pos + 1), body => seen.push(body));
  decoder.finish();
  assert.deepEqual(seen.map(body => body.kind), ["session.abort", "worker.shutdown"]);

  const broken = new LineDecoder(parseInbound);
  assert.throws(() => broken.push(Buffer.from("{oops}\n"), () => {}), { code: "invalid_json" });
  assert.throws(() => broken.push(Buffer.concat([frame({ kind: "session.abort" }), Buffer.from("\n")]), () => {}), { code: "closed" });

  const partial = new LineDecoder(parseInbound);
  partial.push(frame({ kind: "session.abort" }), () => {});
  assert.throws(() => partial.finish(), { code: "truncated_frame" });
});

test("outbound frames are bounded before they are written", () => {
  assert.match(encodeOutbound({ kind: "turn.state", state: "idle" }).toString(), /^\{"protocol":1,"bundle":"0\.2\.0","body":\{"kind":"turn.state","state":"idle"\}\}\n$/);
  assert.throws(() => encodeOutbound({ kind: "worker.error", code: "internal", message: "m".repeat(513) }), { code: "invalid_schema" });
  assert.throws(() => encodeOutbound({ kind: "activity.delta", item_id: ID, append_text: "a".repeat(MAX_STRING + 1) }), { code: "limit_reached" });
});

test("item text is bounded by bytes, UTF-8 boundaries and the encoded frame size", () => {
  const wide = boundText("é".repeat(MAX_ITEM_TEXT_BYTES));
  assert.equal(wide.truncated, true);
  assert.ok(Buffer.byteLength(wide.text) <= MAX_ITEM_TEXT_BYTES);
  assert.equal(wide.text.includes("�"), false);

  const control = boundText("\u0001".repeat(MAX_ITEM_TEXT_BYTES));
  assert.equal(control.truncated, true);
  assert.ok(encodeOutbound({ kind: "activity.item", item_id: ID, role: "tool", title: "t", text: control.text, is_error: false, truncated: true }).length < MAX_FRAME);
  assert.equal(boundText("\uD800").text, "�", "lone surrogates are normalised before JSON encoding");
  assert.equal(safeMessage(`401: key sk-${"a".repeat(40)}\nrest`), "401: key [redacted] rest");
});

test("streamed text is normalized and bounded before frame encoding", () => {
  const message: AssistantMessage = {
    role: "assistant", content: [], api: "test", provider: "test", model: "test", stopReason: "pending", timestamp: 0,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  };

  for (const delta of ["\uD800a", "\u0001".repeat(MAX_ITEM_TEXT_BYTES)]) {
    const sent: WorkerToService[] = [];
    const mapper = new ActivityMapper(body => { encodeOutbound(body); sent.push(body); });
    assert.doesNotThrow(() => mapper.handle({ type: "message_update", message, assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta, partial: message } }));
    const streamed = sent.find(body => body.kind === "activity.delta");
    assert.ok(streamed?.kind === "activity.delta");
    assert.equal(streamed.append_text, boundText(delta).text);
  }

  const sent: WorkerToService[] = [];
  const mapper = new ActivityMapper(body => { encodeOutbound(body); sent.push(body); });

  for (const delta of ["before \uD83D", "\uDE00 after"]) mapper.handle({ type: "message_update", message, assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta, partial: message } });
  const completed: AssistantMessage = { ...message, content: [{ type: "text", text: "before 😀 after" }], stopReason: "stop" };
  mapper.handle({ type: "message_end", message: completed });
  const deltas = sent.flatMap(body => body.kind === "activity.delta" ? [body] : []);
  const final = sent.at(-1);
  assert.equal(deltas.map(body => body.append_text).join(""), "before 😀 after");
  assert.ok(final?.kind === "activity.item");
  assert.equal(final.text, "before 😀 after");
  assert.ok(deltas.every(body => body.item_id === final.item_id));
});

test("delta coalescing keeps control-heavy text within the frame bound", async () => {
  const drains = new EventEmitter();
  const written: string[] = [];
  let full = true;

  const channel = new StdioChannel(Readable.from([]), bytes => { written.push(bytes.toString());

 return !full; }, drains);

  const running = channel.send({ kind: "turn.state", state: "running" });
  const first = channel.send({ kind: "activity.delta", item_id: ID, append_text: "\u0001".repeat(60 * 1024) });
  const second = channel.send({ kind: "activity.delta", item_id: ID, append_text: "\u0001".repeat(130 * 1024) });
  full = false;
  drains.emit("drain");
  await assert.doesNotReject(Promise.all([running, first, second]));
  assert.equal(written.length, 3);
});

test("coalesced output below the queue budget retains every delta", async () => {
  const drains = new EventEmitter();
  let full = true;
  let retained = "";

  const channel = new StdioChannel(Readable.from([]), bytes => {
    const frame = decodeWorkerFrame(bytes.subarray(0, -1));

    if (frame.body.kind === "activity.delta") retained += frame.body.append_text;

    return !full;
  }, drains);

  const pending = [channel.send({ kind: "turn.state", state: "running" })];
  const delta = "x".repeat(1024);
  let itemId = ID;

  for (let n = 0; n < 3800; n++) {
    if (n % 190 === 0) itemId = newId();
    pending.push(channel.send({ kind: "activity.delta", item_id: itemId, append_text: delta }));
  }

  full = false;
  drains.emit("drain");
  await Promise.all(pending);
  assert.equal(retained.length, 3800 * delta.length);
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

test("frames queued while a drain settles are still written", async () => {
  const written: string[] = [];

  const channel = new StdioChannel(Readable.from([]), bytes => { written.push(bytes.toString());

 return true; }, new EventEmitter());

  void channel.send({ kind: "turn.state", state: "running" });
  void channel.send({ kind: "turn.state", state: "idle" });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(written.length, 2, "two synchronous sends are both written without a third");
});

test("character-bounded titles, labels and messages never end in half a surrogate pair", async () => {
  const emoji = "\u{1F600}";
  const sent: WorkerToService[] = [];
  const mapper = new ActivityMapper(body => { sent.push(body); });
  mapper.handle({ type: "tool_execution_start", toolCallId: "call-1", toolName: "bash", args: { command: emoji.repeat(100) } });
  mapper.system(emoji.repeat(150), "text", false);
  sent.push({ kind: "worker.error", code: "internal", message: safeMessage(emoji.repeat(300)) });
  const entry = treeEntry({ type: "message", id: "e1", parentId: null, timestamp: "2026-01-01T00:00:00.000Z", message: { role: "user", content: emoji.repeat(150), timestamp: 0 } }, emoji.repeat(150));
  sent.push({ kind: "session.result", command_id: ID, ok: true, message: "", entries: [entry] });
  const ui = new UiBridge(body => { sent.push(body); }, () => {});
  const answer = ui.context().input(`i${emoji.repeat(300)}`, `p${emoji.repeat(300)}`);
  const request = sent.at(-1);
  assert.equal(request?.kind, "ui.request");
  ui.advance();
  assert.equal(await answer, undefined);
  assert.ok(sent.length >= 5);

  for (const body of sent) assert.doesNotThrow(() => encodeOutbound(body), `${body.kind} encodes`);
});

test("select answers map back to the full option and an empty choice is refused", async () => {
  const sent: WorkerToService[] = [];
  const ui = new UiBridge(body => { sent.push(body); }, () => {});
  const long = `${"o".repeat(600)}`;
  const chosen = ui.context().select("pick", ["short", long]);
  const request = sent.at(-1);
  assert.ok(request?.kind === "ui.request" && request.prompt.kind === "select");
  const shown = request.prompt.options[1] ?? "";
  assert.equal(ui.respond({ kind: "ui.respond", ui_id: request.ui_id, generation: request.generation, answer: { kind: "value", value: shown } }), undefined);
  assert.equal(await chosen, long, "a clipped option still selects the option the extension offered");
  await assert.rejects(ui.context().select("pick", []), /at least one option/);
  assert.equal(sent.length, 1, "no unencodable dialog request is sent");
});

test("select options that share a clipped prefix remain separately selectable", async () => {
  const sent: WorkerToService[] = [];
  const ui = new UiBridge(body => { sent.push(body); }, () => {});
  const common = "o".repeat(600);
  const options = [`${common} first`, `${common} second`];
  const chosen = ui.context().select("pick", options);
  const request = sent.at(-1);
  assert.ok(request?.kind === "ui.request" && request.prompt.kind === "select");
  assert.equal(new Set(request.prompt.options).size, 2, "clipping must not merge distinct choices");
  const shown = request.prompt.options[1];
  assert.ok(shown !== undefined);
  assert.doesNotThrow(() => encodeOutbound(request));
  assert.equal(ui.respond({ kind: "ui.respond", ui_id: request.ui_id, generation: request.generation, answer: { kind: "value", value: shown } }), undefined);
  assert.equal(await chosen, options[1]);
});

test("a tree page of long labelled entries still fits one session.result frame", () => {
  const wide = "\u6f22".repeat(300);
  const entries = Array.from({ length: MAX_TREE_ENTRIES }, (_, n) => ({ type: "message" as const, id: `e${n}`, parentId: n === 0 ? null : `e${n - 1}`, timestamp: "2026-01-01T00:00:00.000Z", message: { role: "user" as const, content: wide, timestamp: n } }));
  const all = entries.map(entry => treeEntry(entry, wide));
  assert.throws(() => encodeOutbound({ kind: "session.result", command_id: ID, ok: true, message: "", entries: all }), { code: "frame_size" }, "the unbounded page would not fit");
  const page = treePage(entries, () => wide);
  assert.equal(page.truncated, true);
  assert.ok(page.entries.length > 0 && page.entries.length < entries.length);
  assert.equal(page.entries.at(-1)?.entry_id, `e${MAX_TREE_ENTRIES - 1}`, "the newest entries are kept, oldest first");
  assert.doesNotThrow(() => encodeOutbound({ kind: "session.result", command_id: ID, ok: true, message: "", entries: page.entries, truncated: true, leaf_id: "e1023" }));
  assert.deepEqual(treePage(entries.slice(0, 3), () => undefined), { entries: entries.slice(0, 3).map(entry => treeEntry(entry, undefined)), truncated: false });
});

test("path containment compares whole segments", () => {
  assert.equal(isInside("/s/imports", "/s/imports/..backup.jsonl"), true, "a name that begins with .. is inside");
  assert.equal(isInside("/s/imports", "/s/imports/a/b.jsonl"), true);
  assert.equal(isInside("/s/imports", "/s/imports/../b.jsonl"), false);
  assert.equal(isInside("/s/imports", "/s/imports"), false, "the parent itself is not inside");
  assert.equal(isInside("/s/imports", "/s/imports-other/b.jsonl"), false);
});
