import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { MAX_ITEM_BYTES } from "../src/worker/activity.ts";
import { FIXTURE_SWITCH } from "../src/worker/fixture.ts";
import { cleanup, deliverBody, envelope, record, sandbox, startBody, text, Worker } from "./worker-support.ts";
import type { Sandbox } from "./worker-support.ts";

function userTexts(worker: Worker, from = 0): string[] {
  return worker.items(from).filter(item => item.role === "user").map(item => text(item.text));
}

async function withWorker(run: (worker: Worker, box: Sandbox) => Promise<void>): Promise<void> {
  const box = sandbox();
  const worker = new Worker(box);

  try {
    await run(worker, box);
  } finally {
    worker.child.kill("SIGKILL");
    await worker.exited;
    cleanup(box);
  }
}

test("worker.start creates an isolated session and reports worker.ready", () => withWorker(async (worker, box) => {
  const ready = await worker.start(box);
  assert.ok(text(ready.pi_session_id).length > 0);
  assert.ok(text(ready.session_file).startsWith(join(box.sessionDir, "sessions")));
  assert.ok(existsSync(join(box.sessionDir, "agent")));
  assert.equal(existsSync(join(box.home, ".pi")), false, "the real profile location is never created");
  assert.equal(await worker.shutdown(), 0);
  assert.equal(worker.kinds("worker.error").length, 0);
}));

test("a write directive runs the real write tool in cwd and streams activity", () => withWorker(async (worker, box) => {
  await worker.start(box);
  const mark = worker.frames.length;
  const inputId = randomUUID();
  worker.send(deliverBody("prompt", "Please create the file.\nlade-fixture: write notes/hello.txt Hello\\nWorld", inputId));
  await worker.idle(mark);

  assert.equal(readFileSync(join(box.cwd, "notes", "hello.txt"), "utf8"), "Hello\nWorld");
  const kinds = worker.frames.slice(mark).map(frame => frame.kind);
  assert.equal(kinds[0], "input.received");
  assert.equal(worker.frames[mark]?.input_id, inputId);
  assert.ok(kinds.indexOf("turn.state") > 0);
  assert.ok(kinds.includes("activity.delta"), "assistant text is streamed as deltas");

  const items = worker.items(mark);
  const tool = items.find(item => item.role === "tool");
  assert.ok(tool, "tool execution is an activity item");
  assert.equal(tool.tool_name, "write");
  assert.equal(tool.is_error, false);
  assert.match(text(tool.text), /notes\/hello\.txt/);
  assert.match(text(tool.text), /result:\n.*Successfully wrote/);
  const assistant = items.filter(item => item.role === "assistant").map(item => text(item.text));
  assert.ok(assistant.some(line => line.includes("writing 1 file(s)")));
  assert.ok(assistant.some(line => line.includes("1 of 1 tool call(s) succeeded")));
  assert.deepEqual(userTexts(worker, mark), ["Please create the file.\nlade-fixture: write notes/hello.txt Hello\\nWorld"]);

  for (const item of items.filter(entry => entry.role === "assistant")) {
    const deltas = worker.frames.filter(frame => frame.kind === "activity.delta" && frame.item_id === item.item_id).map(frame => text(frame.append_text)).join("");
    assert.equal(deltas, item.text);
  }

  assert.equal(await worker.shutdown(), 0);
}));

test("steer and follow-up during a running turn are received and delivered in order", () => withWorker(async (worker, box) => {
  await worker.start(box);
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "first\nlade-fixture: delay 150\nlade-fixture: write a.txt one"));
  await worker.waitFor(frame => frame.kind === "turn.state" && frame.state === "running", mark);
  const steer = randomUUID();
  const follow = randomUUID();
  worker.send(deliverBody("follow_up", "later please", follow));
  worker.send(deliverBody("steer", "change course", steer));
  await worker.waitFor(frame => frame.kind === "input.received" && frame.input_id === steer, mark);
  await worker.idle(mark);

  const received = worker.kinds("input.received").map(frame => frame.input_id);
  assert.ok(received.includes(steer) && received.includes(follow));
  assert.deepEqual(userTexts(worker, mark), ["first\nlade-fixture: delay 150\nlade-fixture: write a.txt one", "change course", "later please"]);
  assert.equal(worker.kinds("turn.state").filter(frame => frame.state === "idle").length, 1, "one settled run covers queued input");
  assert.equal(readFileSync(join(box.cwd, "a.txt"), "utf8"), "one");
}));

test("a prompt while streaming is queued as a follow-up", () => withWorker(async (worker, box) => {
  await worker.start(box);
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "one\nlade-fixture: delay 100"));
  await worker.waitFor(frame => frame.kind === "turn.state" && frame.state === "running", mark);
  worker.send(deliverBody("prompt", "two"));
  await worker.idle(mark);
  assert.deepEqual(userTexts(worker, mark), ["one\nlade-fixture: delay 100", "two"]);
  assert.equal(worker.kinds("worker.error").length, 0);
}));

test("session.abort clears queued input, stops the run and keeps the worker usable", () => withWorker(async (worker, box) => {
  await worker.start(box);
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "slow\nlade-fixture: delay 400\nlade-fixture: write never.txt x"));
  await worker.waitFor(frame => frame.kind === "turn.state" && frame.state === "running", mark);
  worker.send(deliverBody("follow_up", "queued follow-up"));
  worker.send({ kind: "session.abort" });
  await worker.idle(mark);

  const items = worker.items(mark);
  assert.ok(items.some(item => item.role === "system" && item.title === "Queue cleared"));
  assert.ok(items.some(item => item.role === "system" && item.title === "Turn aborted"));
  assert.deepEqual(userTexts(worker, mark), ["slow\nlade-fixture: delay 400\nlade-fixture: write never.txt x"]);
  assert.equal(existsSync(join(box.cwd, "never.txt")), false);

  const next = worker.frames.length;
  worker.send(deliverBody("prompt", "again"));
  await worker.idle(next);
  assert.ok(worker.items(next).some(item => item.role === "assistant" && text(item.text).includes("scripted reply")));
  assert.equal(await worker.shutdown(), 0);
}));

test("resume reopens the same pi session with its history", async () => {
  const box = sandbox();

  try {
    const first = new Worker(box);
    const ready = await first.start(box);
    first.send(deliverBody("prompt", "remember this"));
    await first.idle(0);
    assert.equal(await first.shutdown(), 0);
    assert.ok(existsSync(text(ready.session_file)), "the session file is flushed after the assistant reply");

    const second = new Worker(box);
    const resumed = await second.start(box, true);
    assert.equal(resumed.pi_session_id, ready.pi_session_id);
    assert.equal(resumed.session_file, ready.session_file);
    const mark = second.frames.length;
    second.send(deliverBody("prompt", "and now?"));
    await second.idle(mark);
    assert.ok(second.items(mark).some(item => item.role === "assistant" && text(item.text).includes("holds 2 user message(s)")));
    assert.equal(await second.shutdown(), 0);
  } finally {
    cleanup(box);
  }
});

test("an invalid frame terminates the worker without echoing input", () => withWorker(async (worker, box) => {
  await worker.start(box);
  const secret = "sk-SECRET-should-never-echo-0123456789";
  worker.sendRaw(`{"protocol":1,"bundle":"0.2.0","body":{"kind":"input.deliver","input_id":"${secret}","mode":"prompt","text":"${secret}"}}\n`);
  assert.equal(await worker.exited, 2);
  const errors = worker.kinds("worker.error");
  assert.equal(errors.length, 1);
  assert.equal(errors[0]?.code, "protocol_violation");
  assert.equal(worker.stdout.includes(secret), false);
  assert.equal(worker.stderr.includes(secret), false);
}));

test("frames before worker.start and unknown fields are protocol errors", () => withWorker(async worker => {
  worker.sendRaw(envelope({ kind: "input.deliver", input_id: randomUUID(), mode: "prompt", text: "x" }));
  assert.equal(await worker.exited, 2);
  assert.equal(worker.kinds("worker.error")[0]?.code, "protocol_violation");
}));

test("a fixture provider failure reports worker.error and the worker stays usable", () => withWorker(async (worker, box) => {
  await worker.start(box);
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "lade-fixture: fail"));
  await worker.idle(mark);
  const error = worker.kinds("worker.error").at(-1);
  assert.equal(error?.code, "provider_failed");
  assert.match(text(error?.message), /scripted provider failure/);
  assert.equal(text(error?.message).includes("lade-fixture: fail\n"), false);

  const next = worker.frames.length;
  worker.send(deliverBody("prompt", "recover"));
  await worker.idle(next);
  assert.ok(worker.items(next).some(item => item.role === "assistant" && text(item.text).includes("scripted reply")));
  assert.equal(await worker.shutdown(), 0);
}));

test("a tool item over 192 KiB is truncated and flagged", () => withWorker(async (worker, box) => {
  await worker.start(box);
  const mark = worker.frames.length;
  const size = 200 * 1024;
  worker.send(deliverBody("prompt", `lade-fixture: write big.txt ${"x".repeat(size)}`));
  await worker.idle(mark);
  assert.equal(readFileSync(join(box.cwd, "big.txt"), "utf8").length, size, "the tool still writes the full content");
  const tool = worker.items(mark).find(item => item.role === "tool");
  assert.ok(tool);
  assert.equal(tool.truncated, true);
  assert.match(text(tool.title), /truncated; \d+ bytes/);
  assert.ok(Buffer.byteLength(text(tool.text)) <= MAX_ITEM_BYTES);
  const user = worker.items(mark).find(item => item.role === "user");
  assert.equal(record(user).truncated, true);
}));

test("stdin EOF stops the worker cleanly", () => withWorker(async (worker, box) => {
  await worker.start(box);
  worker.child.stdin.end();
  assert.equal(await worker.exited, 0);
}));

test("an unknown model fails worker.start with a coded error", () => withWorker(async (worker, box) => {
  worker.send({ ...startBody(box, false), model: { provider: "lade-missing", id: "none" } });
  assert.equal(await worker.exited, 1);
  assert.equal(worker.kinds("worker.error")[0]?.code, "startup_failed");
  assert.equal(worker.kinds("worker.ready").length, 0);
}));

test("worker.start refuses a launch environment that does not match its session_dir", async () => {
  const box = sandbox();
  const worker = new Worker(box, { PI_CODING_AGENT_DIR: join(box.root, "elsewhere") });

  try {
    worker.send(startBody(box, false));
    assert.equal(await worker.exited, 1);
    const error = record(worker.kinds("worker.error")[0]);
    assert.equal(error.code, "startup_failed");
    assert.match(text(error.message), /PI_CODING_AGENT_DIR/);
    assert.equal(existsSync(join(box.sessionDir, "agent")), false, "nothing is created before the environment is checked");
  } finally {
    worker.child.kill("SIGKILL");
    await worker.exited;
    cleanup(box);
  }
});

test("without the test switch the fixture provider is not registered and worker.start is refused", async () => {
  const box = sandbox();
  const worker = new Worker(box, { [FIXTURE_SWITCH]: "" });

  try {
    worker.send(startBody(box, false));
    assert.equal(await worker.exited, 1);
    const error = record(worker.kinds("worker.error")[0]);
    assert.equal(error.code, "startup_failed");
    assert.match(text(error.message), /not registered/);
    assert.equal(worker.kinds("worker.ready").length, 0);
  } finally {
    worker.child.kill("SIGKILL");
    await worker.exited;
    cleanup(box);
  }
});
