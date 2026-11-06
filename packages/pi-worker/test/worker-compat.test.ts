import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { FIXTURE_PROVIDER, FIXTURE_SWITCH } from "../src/worker/fixture.ts";
import { extensionHash } from "../src/worker/resources.ts";
import type { JsonObject, JsonValue } from "../src/json.ts";
import { cleanup, deliverBody, envelope, record, sandbox, startBody, text, TIMEOUT_MS, Worker, WORKER } from "./worker-support.ts";
import type { Sandbox } from "./worker-support.ts";

// The input handler's dialog waits as long as the test does, so a slow host cannot time it out before the answer;
// the timeout close itself is covered by `/ask quick`.
const EXTENSION = `export default function (pi) {
  pi.registerCommand("ask", {
    description: "LADE fixture confirmation",
    handler: async (args, ctx) => {
      const ok = await ctx.ui.confirm("Proceed?", "Fixture confirmation " + args, args === "quick" ? { timeout: 300 } : undefined);
      pi.sendUserMessage("lade-fixture: echo confirmed=" + ok);
    },
  });
  pi.registerCommand("pick", {
    description: "LADE fixture selection",
    handler: async (_args, ctx) => {
      const choice = await ctx.ui.select("Pick one", ["red", "green"]);
      pi.sendUserMessage("lade-fixture: echo picked=" + choice);
    },
  });
  pi.registerCommand("paint", {
    description: "LADE fixture terminal-only component",
    handler: async (_args, ctx) => {
      await ctx.ui.custom(() => ({ render: () => [], invalidate: () => {} }));
      pi.sendUserMessage("lade-fixture: echo painted");
    },
  });
  pi.on("input", async (event, ctx) => {
    if (event.text !== "confirm-input") return { action: "continue" };
    const ok = await ctx.ui.confirm("Accept input?", "Confirm before the turn starts", { timeout: ${TIMEOUT_MS} });
    return { action: "transform", text: "lade-fixture: echo confirmed=" + ok };
  });
}
`;

/** A long turn, so that several of them leave the session something to compact. */
const LONG_TURN = "context ".repeat(4_000);

interface Profile { dir: string; extension: string }

function write(path: string, content: string): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content);
}

function profile(box: Sandbox): Profile {
  const dir = join(box.sessionDir, "profile");
  write(join(dir, "AGENTS.md"), "Profile context LADE-MARKER:profile-context\n");
  write(join(dir, "skills", "demo", "SKILL.md"), "---\nname: demo\ndescription: Demo skill LADE-MARKER:skill-demo\n---\nUse the demo skill.\n");
  write(join(dir, "prompts", "twice.md"), "/twice $1-again\nlade-fixture: echo\n");
  write(join(dir, "extensions", "ask.js"), EXTENSION);
  write(join(box.cwd, "AGENTS.md"), "Repository context LADE-MARKER:repo-context\n");
  write(join(box.cwd, ".pi", "skills", "evil", "SKILL.md"), "---\nname: evil\ndescription: Repository skill LADE-MARKER:repo-pi\n---\nx\n");
  write(join(box.cwd, ".pi", "extensions", "evil.js"), "export default function () { throw new Error('repository extension loaded'); }\n");
  write(join(box.sessionDir, "agent", "skills", "agentdir", "SKILL.md"), "---\nname: agentdir\ndescription: Agent dir skill LADE-MARKER:agent-dir\n---\nx\n");

  return { dir, extension: join(dir, "extensions", "ask.js") };
}

function resources(found: Profile, sha256 = extensionHash(found.extension)): JsonObject {
  return { profile_dir: found.dir, extensions: [{ name: "ask.js", path: found.extension, sha256 }] };
}

function list(value: JsonValue | undefined): JsonObject[] {
  return (Array.isArray(value) ? value : []).map(entry => record(entry));
}

function assistant(worker: Worker, from: number): string[] {
  return worker.items(from).filter(item => item.role === "assistant").map(item => text(item.text));
}

async function prompt(worker: Worker, body: string): Promise<number> {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", body));
  await worker.waitForIdle(mark);

  return mark;
}

function command(worker: Worker, payload: JsonObject): Promise<JsonObject> {
  const id = randomUUID();
  const mark = worker.frames.length;
  worker.send({ kind: "session.command", command_id: id, command: payload });

  return worker.waitFor(frame => frame.kind === "session.result" && frame.command_id === id, mark);
}

async function withProfile(run: (worker: Worker, box: Sandbox, found: Profile) => Promise<void>, sha256?: string): Promise<void> {
  const box = sandbox();
  const found = profile(box);
  const worker = new Worker(box);

  try {
    worker.send({ ...startBody(box, false), resources: resources(found, sha256) });
    await worker.waitFor(frame => frame.kind === "worker.ready");
    await worker.waitFor(frame => frame.kind === "resources.loaded");
    await run(worker, box, found);
  } finally {
    worker.child.kill("SIGKILL");
    await worker.exited;
    cleanup(box);
  }
}

test("extension hashes match the service's algorithm (shared constants)", () => {
  const box = sandbox();

  try {
    const dir = join(box.root, "ext");
    write(join(dir, "index.js"), "export default function () {}\n");
    write(join(dir, "lib", "util.js"), "export const x = 1;\n");
    assert.equal(extensionHash(join(dir, "index.js")), "4554c00dbb28cad89d67afb301d893073eefca19bee50292764c7b8c3dc8bf03");
    assert.equal(extensionHash(dir), "3649d03b8d53b790fab2b926092c3e3ff4dfac57fbf968ab8c3e8e27b767d73a");
  } finally {
    cleanup(box);
  }
});

test("profile resources load with documented precedence; repository .pi and the agent dir stay untrusted", () => withProfile(async worker => {
  const changed = record(worker.kinds("session.changed")[0]);
  assert.equal(changed.reason, "start");
  assert.equal(changed.generation, "1");
  const loaded = record(worker.kinds("resources.loaded")[0]);
  const entries = list(loaded.resources);
  const names = entries.map(entry => `${text(entry.kind)}:${text(entry.name)}:${text(entry.source)}:${text(entry.status)}`);
  assert.ok(names.includes("skill:demo:lade_profile:loaded"), names.join(","));
  assert.ok(names.includes("prompt:twice:lade_profile:loaded"), names.join(","));
  assert.ok(names.includes("context:AGENTS.md:lade_profile:loaded"));
  assert.ok(names.includes("context:AGENTS.md:repository:loaded"));
  assert.ok(names.includes("extension:ask.js:lade_profile:loaded"));
  assert.equal(names.some(name => name.includes("evil") || name.includes("agentdir")), false);

  const mark = await prompt(worker, "lade-fixture: system");
  const reply = assistant(worker, mark).join("\n");
  assert.match(reply, /LADE-MARKER:profile-context/);
  assert.match(reply, /LADE-MARKER:repo-context/);
  assert.match(reply, /LADE-MARKER:skill-demo/);
  assert.equal(reply.includes("LADE-MARKER:repo-pi"), false, "repository .pi skills are never loaded");
  assert.equal(reply.includes("LADE-MARKER:agent-dir"), false, "the agent dir is not auto-discovered");
  assert.equal(worker.kinds("worker.error").length, 0);
}));

test("a prompt template is expanded exactly once", () => withProfile(async worker => {
  const mark = await prompt(worker, "/twice x");
  const user = worker.items(mark).filter(item => item.role === "user").map(item => text(item.text));
  assert.deepEqual(user, ["/twice x-again\nlade-fixture: echo\n"]);
  const reply = assistant(worker, mark).join("\n");
  assert.match(reply, /\/twice x-again/);
  assert.equal(reply.includes("x-again-again"), false);
}));

test("an extension whose content changed since approval is refused and never loaded", () => withProfile(async worker => {
  const loaded = record(worker.kinds("resources.loaded")[0]);
  const extension = list(loaded.resources).find(entry => entry.kind === "extension");
  assert.equal(extension?.status, "refused");
  assert.match(text(extension?.message), /changed since it was approved/);
  const mark = await prompt(worker, "/ask now");
  assert.equal(worker.kinds("ui.request").length, 0, "the unapproved command does not exist");
  assert.ok(assistant(worker, mark).some(line => line.includes("scripted reply")));
}, "0".repeat(64)));

test("extension dialogs are routed with a generation and answered only once", () => withProfile(async worker => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "/ask now"));
  const request = await worker.waitFor(frame => frame.kind === "ui.request", mark);
  assert.equal(request.generation, "1");
  assert.equal(record(request.prompt).kind, "confirm");
  assert.equal(record(request.prompt).message, "Fixture confirmation now");

  worker.send({ kind: "ui.respond", ui_id: text(request.ui_id), generation: "2", answer: { kind: "confirm", confirmed: true } });
  await worker.waitFor(frame => frame.kind === "activity.item" && frame.title === "Dialog answer refused", mark);
  worker.send({ kind: "ui.respond", ui_id: text(request.ui_id), generation: "1", answer: { kind: "confirm", confirmed: true } });
  await worker.waitFor(frame => frame.kind === "turn.state" && frame.state === "idle", mark);
  await worker.waitFor(() => assistant(worker, mark).some(line => line.includes("confirmed=true")), mark);

  const late = worker.frames.length;
  worker.send({ kind: "ui.respond", ui_id: text(request.ui_id), generation: "1", answer: { kind: "confirm", confirmed: false } });
  await worker.waitFor(frame => frame.kind === "activity.item" && frame.title === "Dialog answer refused", late);
}));

test("a dialog is never answered automatically: timeout closes it as a refusal", () => withProfile(async worker => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "/ask quick"));
  const request = await worker.waitFor(frame => frame.kind === "ui.request", mark);
  const closed = await worker.waitFor(frame => frame.kind === "ui.closed" && frame.ui_id === request.ui_id, mark);
  assert.equal(closed.reason, "timeout");
  await worker.waitFor(() => assistant(worker, mark).some(line => line.includes("confirmed=false")), mark);
}));

test("a dialog opened by an input handler can receive its answer before preflight finishes", () => withProfile(async worker => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "confirm-input"));
  const request = await worker.waitFor(frame => frame.kind === "ui.request", mark);
  worker.send({ kind: "ui.respond", ui_id: text(request.ui_id), generation: text(request.generation), answer: { kind: "confirm", confirmed: true } });
  await worker.waitForIdle(mark);
  assert.ok(assistant(worker, mark).some(line => line.includes("confirmed=true")), "the answer must reach the input handler while it is waiting");
  assert.equal(worker.kinds("ui.closed").some(frame => frame.ui_id === request.ui_id && frame.reason === "timeout"), false);
}));

test("inputs delivered while preflight waits for a dialog retain their order", () => withProfile(async worker => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "confirm-input"));
  const request = await worker.waitFor(frame => frame.kind === "ui.request", mark);
  const later = randomUUID();
  worker.send(deliverBody("prompt", "later input", later));
  await worker.waitFor(frame => frame.kind === "input.received" && frame.input_id === later, mark);
  worker.send({ kind: "ui.respond", ui_id: text(request.ui_id), generation: text(request.generation), answer: { kind: "confirm", confirmed: true } });
  await worker.waitForIdle(mark);
  const users = worker.items(mark).filter(item => item.role === "user").map(item => text(item.text));
  assert.deepEqual(users, ["lade-fixture: echo confirmed=true", "later input"]);
  assert.equal(worker.kinds("worker.error").length, 0);
}));

test("abort closes a preflight dialog and discards its queued input", () => withProfile(async worker => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "confirm-input"));
  const request = await worker.waitFor(frame => frame.kind === "ui.request", mark);
  worker.send(deliverBody("prompt", "queued before abort"));
  worker.send({ kind: "session.abort" });
  worker.send(deliverBody("prompt", "after abort"));
  await worker.waitForIdle(mark);
  const users = worker.items(mark).filter(item => item.role === "user").map(item => text(item.text));
  assert.deepEqual(users, ["after abort"]);
  assert.equal(worker.kinds("ui.closed").some(frame => frame.ui_id === request.ui_id && frame.reason === "aborted"), true);
}));

test("session replacement is refused during input preflight and works after its answer", () => withProfile(async (worker, _box, found) => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "confirm-input"));
  const request = await worker.waitFor(frame => frame.kind === "ui.request", mark);
  const waiting = await command(worker, { op: "reload", resources: resources(found) });
  assert.equal(waiting.ok, false);
  assert.match(text(waiting.message), /input delivery is in progress/);
  worker.send({ kind: "ui.respond", ui_id: text(request.ui_id), generation: text(request.generation), answer: { kind: "confirm", confirmed: true } });
  await worker.waitForIdle(mark);
  assert.ok(assistant(worker, mark).some(line => line.includes("confirmed=true")));
  const reloaded = await command(worker, { op: "reload", resources: resources(found) });
  assert.equal(reloaded.ok, true, text(reloaded.message));
}));

test("shutdown closes a preflight dialog without starting its turn", () => withProfile(async worker => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "confirm-input"));
  const request = await worker.waitFor(frame => frame.kind === "ui.request", mark);
  assert.equal(await worker.shutdown(), 0);
  assert.equal(worker.kinds("ui.closed").some(frame => frame.ui_id === request.ui_id && frame.reason === "aborted"), true);
  assert.equal(worker.items(mark).some(item => item.role === "user"), false);
}));

test("terminal-only extension UI fails explicitly", () => withProfile(async worker => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "/paint"));
  const error = await worker.waitFor(frame => frame.kind === "activity.item" && frame.title === "Extension error", mark);
  assert.match(text(error.text), /does not support the extension UI surface "custom"/);
  assert.equal(assistant(worker, mark).some(line => line.includes("painted")), false);
}));

test("reload starts a new generation, closes open dialogs and rejects their late answers", () => withProfile(async (worker, _box, found) => {
  const mark = worker.frames.length;
  worker.send(deliverBody("prompt", "/pick"));
  const request = await worker.waitFor(frame => frame.kind === "ui.request", mark);
  assert.equal(record(request.prompt).kind, "select");
  const result = await command(worker, { op: "reload", resources: resources(found) });
  assert.equal(result.ok, true, text(result.message));
  const closed = await worker.waitFor(frame => frame.kind === "ui.closed" && frame.ui_id === request.ui_id, mark);
  assert.equal(closed.reason, "replaced");
  const changed = await worker.waitFor(frame => frame.kind === "session.changed" && frame.reason === "reload", mark);
  assert.equal(changed.generation, "2");
  worker.send({ kind: "ui.respond", ui_id: text(request.ui_id), generation: "1", answer: { kind: "value", value: "red" } });
  await worker.waitFor(frame => frame.kind === "activity.item" && frame.title === "Dialog answer refused", mark);
  assert.equal(assistant(worker, mark).some(line => line.includes("picked=red")), false, "a late answer never reaches the replaced extension");

  writeFileSync(found.extension, `${EXTENSION}\n// edited\n`);
  const stale = await command(worker, { op: "reload", resources: resources(found, "1".repeat(64)) });
  assert.equal(stale.ok, true);
  const loaded = await worker.waitFor(frame => frame.kind === "resources.loaded" && frame.generation === "3", mark);
  const extension = list(loaded.resources).find(entry => entry.kind === "extension");
  assert.equal(extension?.status, "refused");
}));

test("tree navigation is session-only and never rewinds files; fork and clone create new sessions", () => withProfile(async (worker, box) => {
  await prompt(worker, "first\nlade-fixture: write kept.txt one");
  await prompt(worker, "second");
  const tree = await command(worker, { op: "tree" });
  assert.equal(tree.ok, true);
  const entries = list(tree.entries);
  const users = entries.filter(entry => entry.kind === "user");
  assert.equal(users.length, 2);
  assert.equal(text(users[0]?.label), "first lade-fixture: write kept.txt one");
  const first = text(users[0]?.entry_id);
  const second = text(users[1]?.entry_id);
  const before = readFileSync(join(box.cwd, "kept.txt"), "utf8");

  const mark = worker.frames.length;
  const navigated = await command(worker, { op: "navigate", entry_id: first });
  assert.equal(navigated.ok, true, text(navigated.message));
  const change = await worker.waitFor(frame => frame.kind === "session.changed" && frame.reason === "navigate", mark);
  assert.notEqual(change.leaf_id, tree.leaf_id);
  assert.equal(change.generation, "1", "navigation keeps the generation");
  assert.equal(readFileSync(join(box.cwd, "kept.txt"), "utf8"), before, "files are never rewound");
  assert.equal(record((await command(worker, { op: "navigate", entry_id: "nope" }))).ok, false);

  const ready = record(worker.kinds("session.changed")[0]);
  const forked = await command(worker, { op: "fork", entry_id: second, position: "before" });
  assert.equal(forked.ok, true, text(forked.message));
  const fork = await worker.waitFor(frame => frame.kind === "session.changed" && frame.reason === "fork", mark);
  assert.equal(fork.generation, "2");
  assert.notEqual(fork.pi_session_id, ready.pi_session_id);
  const next = await prompt(worker, "after fork");
  assert.ok(assistant(worker, next).some(line => line.includes("holds 2 user message(s)")), "the fork keeps history up to the selected entry");

  const cloneTree = await command(worker, { op: "tree" });
  const clone = await command(worker, { op: "fork", entry_id: text(cloneTree.leaf_id), position: "at" });
  assert.equal(clone.ok, true, text(clone.message));
  const cloned = await worker.waitFor(frame => frame.kind === "session.changed" && frame.reason === "fork" && frame.generation === "3", mark);
  assert.notEqual(cloned.pi_session_id, fork.pi_session_id);
  assert.equal(existsSync(join(box.cwd, "kept.txt")), true);
}));

test("export writes the current branch; import restores conversation metadata only", () => withProfile(async (worker, box) => {
  await prompt(worker, "remember the plan");
  const exportPath = join(box.sessionDir, "exports", "branch.jsonl");
  const exported = await command(worker, { op: "export", path: exportPath });
  assert.equal(exported.ok, true, text(exported.message));
  const header = record(JSON.parse(readFileSync(exportPath, "utf8").split("\n")[0] ?? "{}"));
  assert.equal(header.type, "session");
  assert.equal((await command(worker, { op: "export", path: join(box.root, "outside.jsonl") })).ok, false, "exports stay in the export directory");

  const imported = join(box.sessionDir, "imports", "foreign.jsonl");
  const lines = readFileSync(exportPath, "utf8").trim().split("\n").map(line => record(JSON.parse(line)));
  const foreign = [{ ...lines[0], cwd: "/elsewhere/repository" }, ...lines.slice(1)];
  const last = lines.at(-1);
  foreign.push({ type: "custom", id: "deadbeef", parentId: last?.id ?? null, timestamp: new Date().toISOString(), customType: "lade.approval", data: { approved: true } });
  write(imported, `${foreign.map(line => JSON.stringify(line)).join("\n")}\n`);
  const original = readFileSync(imported, "utf8");

  const mark = worker.frames.length;
  const result = await command(worker, { op: "import", path: imported });
  assert.equal(result.ok, true, text(result.message));
  const change = await worker.waitFor(frame => frame.kind === "session.changed" && frame.reason === "import", mark);
  assert.equal(change.generation, "2");
  assert.equal(readFileSync(imported, "utf8"), original, "the import source is never modified");
  const tree = await command(worker, { op: "tree" });
  const kinds = (Array.isArray(tree.entries) ? tree.entries : []).map(entry => record(entry).kind);
  assert.ok(kinds.includes("custom"), "imported metadata is preserved");
  const after = await prompt(worker, "lade-fixture: write proof.txt here");
  assert.ok(assistant(worker, after).some(line => line.includes("1 of 1 tool call(s) succeeded")));
  assert.equal(readFileSync(join(box.cwd, "proof.txt"), "utf8"), "here", "the imported session works in the Objective worktree");
  assert.equal((await command(worker, { op: "import", path: exportPath })).ok, false, "imports are read only from the import directory");
  copyFileSync(exportPath, join(box.root, "unused.jsonl"));
}));

test("manual compaction, model/thinking configuration and retries surface as activity", () => withProfile(async worker => {
  for (const turn of ["one", "two", "three", "four"]) await prompt(worker, `${turn} ${LONG_TURN}`);
  const mark = worker.frames.length;
  const compacted = await command(worker, { op: "compact", instructions: "keep it short" });

  assert.equal(compacted.ok, true, text(compacted.message));
  await worker.waitFor(frame => frame.kind === "session.changed" && frame.reason === "compact", mark);
  assert.ok(worker.items(mark).some(item => item.title === "Context compacted"));

  const configured = await command(worker, { op: "configure", model: { provider: "lade-fixture", id: "scripted" }, thinking: "high" });
  assert.equal(configured.ok, true, text(configured.message));
  const change = await worker.waitFor(frame => frame.kind === "session.changed" && frame.reason === "configure", mark);
  assert.equal(change.thinking, "off", "the fixture model has no reasoning: pi clamps the level");
  assert.equal((await command(worker, { op: "configure", model: { provider: "lade-fixture", id: "missing" }, thinking: "off" })).ok, false);

  const retry = await prompt(worker, "lade-fixture: transient");
  const titles = worker.items(retry).map(item => text(item.title));
  assert.ok(titles.includes("Retrying model request"), titles.join(","));
  assert.ok(titles.includes("Retry succeeded"), titles.join(","));
  assert.ok(assistant(worker, retry).some(line => line.includes("scripted reply")));
}));

test("a second command while one runs fails at once; a foreign profile path is refused", () => withProfile(async worker => {
  const outside = await command(worker, { op: "reload", resources: { profile_dir: "/", extensions: [] } });
  assert.equal(outside.ok, false, "a profile outside the session dir is refused");

  for (const turn of ["one", "two", "three", "four"]) await prompt(worker, `${turn} ${LONG_TURN}`);
  const first = randomUUID();
  const second = randomUUID();
  const mark = worker.frames.length;
  worker.sendRaw(envelope({ kind: "session.command", command_id: first, command: { op: "compact" } }) + envelope({ kind: "session.command", command_id: second, command: { op: "tree" } }));
  const busy = await worker.waitFor(frame => frame.kind === "session.result" && frame.command_id === second, mark);
  assert.equal(busy.ok, false);
  assert.match(text(busy.message), /another session command is in progress/);
  const done = await worker.waitFor(frame => frame.kind === "session.result" && frame.command_id === first, mark);
  assert.equal(done.ok, true, text(done.message));
}));

function runCatalog(box: Sandbox, name: string, env: Record<string, string>): JsonObject {
  const out = join(box.root, name);
  execFileSync(process.execPath, [join(dirname(WORKER), "catalog.ts"), out], { stdio: "ignore", env: { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: box.home, ...env } });

  return record(JSON.parse(readFileSync(out, "utf8")));
}

test("the pinned compatibility manifest matches the SDK catalogue", () => {
  const box = sandbox();

  try {
    const catalog = runCatalog(box, "catalog.json", {});
    const manifest = record(JSON.parse(readFileSync(join(dirname(WORKER), "..", "..", "..", "docs", "implementation", "pi-compatibility.json"), "utf8")));

    const ids = (value: JsonValue | undefined): string[] => {
      assert.ok(Array.isArray(value));

      return value.map(entry => text(record(entry).id));
    };

    assert.equal(record(manifest.sdk).version, catalog.sdk_version, "manifest is pinned to the installed SDK version");
    assert.deepEqual(ids(manifest.providers), ids(catalog.providers), "every catalogue provider is classified, none extra");
    assert.equal(catalog.error, null);
  } finally { cleanup(box); }
});

test("the catalogue lists the fixture provider only with the test switch", () => {
  const box = sandbox();

  try {
    const providers = (catalog: JsonObject): string[] => {
      assert.ok(Array.isArray(catalog.providers));

      return catalog.providers.map(entry => text(record(entry).id));
    };

    assert.ok(!providers(runCatalog(box, "off.json", {})).includes(FIXTURE_PROVIDER), "absent without the switch");
    const on = runCatalog(box, "on.json", { [FIXTURE_SWITCH]: "1" });
    assert.ok(providers(on).includes(FIXTURE_PROVIDER), "present with the switch");
  } finally { cleanup(box); }
});
