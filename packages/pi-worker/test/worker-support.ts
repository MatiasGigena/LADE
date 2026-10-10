import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { boundedJson } from "../src/json.ts";
import type { JsonObject, JsonValue } from "../src/json.ts";
import { WORKER_BUNDLE, WORKER_PROTOCOL } from "../src/worker/wire.ts";

export const WORKER = join(dirname(fileURLToPath(import.meta.url)), "..", "src", "worker.ts");

export const TIMEOUT_MS = 20_000;

export interface Sandbox { root: string; home: string; sessionDir: string; cwd: string }

/** Disposable HOME, session dir and worktree; never the developer's real profile. */
export function sandbox(): Sandbox {
  const root = mkdtempSync(join(tmpdir(), "lade-worker-"));
  const box = { root, home: join(root, "home"), sessionDir: join(root, "session"), cwd: join(root, "worktree") };
  mkdirSync(box.home); mkdirSync(box.cwd);

  return box;
}

export function cleanup(box: Sandbox): void {
  rmSync(box.root, { recursive: true, force: true });
}

function isRecord(value: JsonValue | undefined): value is JsonObject {
  return value !== null && value !== undefined && !Array.isArray(value) && value === Object(value);
}

export function record(value: JsonValue | undefined): JsonObject {
  assert.ok(isRecord(value), "frame value must be an object");

  return value;
}

export function text(value: JsonValue | undefined): string {
  assert.equal(String(value), value, "expected a string field");

  return String(value);
}

export function envelope(body: JsonObject): string {
  return `${JSON.stringify({ protocol: WORKER_PROTOCOL, bundle: WORKER_BUNDLE, body })}\n`;
}

export function startBody(box: Sandbox, resume: boolean): JsonObject {
  return {
    kind: "worker.start",
    objective_id: randomUUID(),
    session_dir: box.sessionDir,
    cwd: box.cwd,
    model: { provider: "lade-fixture", id: "scripted" },
    resume,
    system_note: "LADE test objective.",
  };
}

export function deliverBody(mode: "prompt" | "steer" | "follow_up", body: string, inputId: string = randomUUID()): JsonObject {
  return { kind: "input.deliver", input_id: inputId, mode, text: body };
}

/** A worker child process over real JSONL pipes; every stdout line must be a protocol frame. */
export class Worker {
  readonly child: ChildProcessWithoutNullStreams;
  readonly frames: JsonObject[] = [];
  readonly exited: Promise<number | null>;
  stdout = "";
  stderr = "";
  private pending = "";
  private wake: () => void = () => {};
  constructor(box: Sandbox, env: Record<string, string> = {}) {
    this.child = spawn(process.execPath, [WORKER], {
      cwd: box.cwd,
      env: { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: box.home, LANG: "C.UTF-8", TERM: "dumb", ...env },
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child.stdout.setEncoding("utf8");
    this.child.stderr.setEncoding("utf8");
    this.child.stdout.on("data", (chunk: string) => {
      this.stdout += chunk; this.pending += chunk;

      for (let end = this.pending.indexOf("\n"); end >= 0; end = this.pending.indexOf("\n")) {
        const frame = record(boundedJson(Buffer.from(this.pending.slice(0, end))));
        this.pending = this.pending.slice(end + 1);
        assert.deepEqual(Object.keys(frame).sort(), ["body", "bundle", "protocol"]);
        assert.equal(frame.protocol, WORKER_PROTOCOL);
        assert.equal(frame.bundle, WORKER_BUNDLE);
        this.frames.push(record(frame.body));
      }

      this.wake();
    });
    this.child.stderr.on("data", (chunk: string) => { this.stderr += chunk; });
    this.exited = new Promise(resolve => this.child.once("exit", code => { this.wake(); resolve(code); }));
  }

  send(body: JsonObject): void {
    this.child.stdin.write(envelope(body));
  }

  sendRaw(bytes: string): void {
    this.child.stdin.write(bytes);
  }

  kinds(kind: string): JsonObject[] {
    return this.frames.filter(frame => frame.kind === kind);
  }

  /** Wait until `predicate` holds for some frame at index >= `from`; returns that frame. */
  async waitFor(predicate: (frame: JsonObject) => boolean, from = 0): Promise<JsonObject> {
    const deadline = Date.now() + TIMEOUT_MS;

    for (;;) {
      const found = this.frames.slice(from).find(predicate);

      if (found) return found;

      if (this.child.exitCode !== null) assert.fail(`worker exited (${this.child.exitCode}) before the expected frame; stderr: ${this.stderr.slice(0, 2000)}`);

      if (Date.now() > deadline) assert.fail(`timed out waiting for a frame; got ${JSON.stringify(this.frames.map(frame => frame.kind))}`);
      await new Promise<void>(resolve => {
        const timer = setTimeout(resolve, 200);
        this.wake = () => { clearTimeout(timer); resolve(); };
      });
    }
  }

  /** Wait for the next `turn.state idle` after frame index `from`. */
  idle(from: number): Promise<JsonObject> {
    return this.waitFor(frame => frame.kind === "turn.state" && frame.state === "idle", from);
  }

  async start(box: Sandbox, resume = false): Promise<JsonObject> {
    this.send(startBody(box, resume));

    return this.waitFor(frame => frame.kind === "worker.ready");
  }

  async shutdown(): Promise<number | null> {
    this.send({ kind: "worker.shutdown" });

    return this.exited;
  }

  /** Final activity.item per item_id, in first-appearance order. */
  items(from = 0): JsonObject[] {
    const latest = new Map<string, JsonObject>();

    for (const frame of this.frames.slice(from)) {
      if (frame.kind === "activity.item") latest.set(text(frame.item_id), frame);
    }

    return [...latest.values()];
  }
}
