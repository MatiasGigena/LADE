import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { findPackageJSON } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";
import type { JsonObject } from "../src/json.ts";
import { FIXTURE_MODEL, FIXTURE_PROVIDER, fixtureProvider } from "../src/worker/fixture.ts";
import { createAssistantMessageEventStream, normalizeContext } from "../src/worker/pi-ai.ts";
import { record, text, WORKER } from "./worker-support.ts";

const PACKAGE = join(dirname(WORKER), "..");

const ROOT = join(PACKAGE, "..", "..");

const SDK = join(PACKAGE, "node_modules", "@earendil-works", "pi-coding-agent");

const SCOPE = join(PACKAGE, "node_modules", "@earendil-works");

const SDK_PATH = "node_modules/@earendil-works/pi-coding-agent";

const EXACT = /^\d+\.\d+\.\d+$/;

function json(path: string): JsonObject {
  return record(JSON.parse(readFileSync(path, "utf8")));
}

function sha256(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function nameOf(path: string): string {
  return path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length);
}

function dependencies(entry: JsonObject): string[] {
  return [entry.dependencies, entry.optionalDependencies, entry.peerDependencies].flatMap(deps => deps === undefined ? [] : Object.keys(record(deps)));
}

function resolveLocked(locked: JsonObject, from: string, name: string): string | undefined {
  let base = from;

  for (;;) {
    const candidate = `${base.length > 0 ? `${base}/` : ""}node_modules/${name}`;

    if (locked[candidate] !== undefined) return candidate;

    if (base.length === 0) return undefined;
    const parent = base.lastIndexOf("/node_modules/");
    base = parent < 0 ? "" : base.slice(0, parent);
  }
}

function closure(locked: JsonObject, root: string): string[] {
  const seen = new Set<string>();
  const pending = [root];

  for (let path = pending.pop(); path !== undefined; path = pending.pop()) {
    if (seen.has(path)) continue;
    seen.add(path);

    for (const name of dependencies(record(locked[path]))) {
      const found = resolveLocked(locked, path, name);

      if (found !== undefined) pending.push(found);
    }
  }

  return [...seen].sort();
}

test("the worker installs exactly one pi-ai: the copy every pi package resolves", () => {
  assert.equal(record(json(join(PACKAGE, "package.json")).dependencies)["@earendil-works/pi-ai"], undefined, "no direct pi-ai dependency");
  const lock = record(json(join(PACKAGE, "package-lock.json")).packages);
  const locked = Object.keys(lock).filter(path => path.endsWith("node_modules/@earendil-works/pi-ai"));
  assert.deepEqual(locked, ["node_modules/@earendil-works/pi-ai"]);
  assert.equal(existsSync(join(SDK, "node_modules", "@earendil-works", "pi-ai")), false, "no nested second copy is installed");

  const hoisted = join(SCOPE, "pi-ai", "package.json");
  const users = Object.entries(lock).filter(([path, entry]) => path.length > 0 && dependencies(record(entry)).includes("@earendil-works/pi-ai")).map(([path]) => path);
  assert.ok(users.includes(SDK_PATH) && users.includes("node_modules/@earendil-works/pi-agent-core"), users.join(", "));

  for (const path of users) assert.equal(findPackageJSON("@earendil-works/pi-ai", pathToFileURL(join(PACKAGE, path, "package.json"))), hoisted, path);

  const shim = readFileSync(join(PACKAGE, "src", "worker", "pi-ai.ts"), "utf8");
  const targets = [...new Set([...shim.matchAll(/from "([^"]+)"/g)].map(match => match[1] ?? ""))];
  assert.equal(targets.length, 1);
  const target = join(PACKAGE, "src", "worker", targets[0] ?? "");
  assert.equal(findPackageJSON(pathToFileURL(target)), hoisted);
  assert.equal(join(SCOPE, "pi-ai", text(record(record(json(hoisted).exports)["."]).import)), target);
});

test("the fixture provider registers on the SDK runtime and streams through the SDK's own pi-ai instance", async () => {
  const scratch = mkdtempSync(join(tmpdir(), "lade-worker-deps-"));

  try {
    const runtime = await ModelRuntime.create({
      authPath: join(scratch, "auth.json"),
      modelsPath: null,
      modelsStorePath: join(scratch, "models-store.json"),
      allowModelNetwork: false,
      refreshOnCreate: false,
    });

    const provider = fixtureProvider();
    runtime.registerProvider(FIXTURE_PROVIDER, provider);
    const model = runtime.getModel(FIXTURE_PROVIDER, FIXTURE_MODEL);
    assert.ok(model, "the fixture model is in the SDK registry");
    const context = normalizeContext({ messages: [{ role: "user", content: "hello", timestamp: Date.now() }] });

    const outer = runtime.streamSimple(model, context);
    const ours = createAssistantMessageEventStream();
    assert.equal(outer.constructor, ours.constructor, "the worker's pi-ai is the instance the SDK uses");
    const direct = provider.streamSimple?.(model, context);
    assert.ok(direct);
    assert.equal(direct.constructor, outer.constructor, "the fixture stream is the SDK's AssistantMessageEventStream");
    await direct.result();

    const message = await outer.result();
    assert.equal(message.stopReason, "stop", message.errorMessage);
    const reply = message.content.map(part => part.type === "text" ? part.text : "").join("");
    assert.match(reply, /^lade-fixture scripted reply \(offline fixture model, not a real model\)\. The context holds 1 user message\(s\)\.$/);
  } finally { rmSync(scratch, { recursive: true, force: true }); }
});

test("the installed SDK is the pinned narrow-fork distribution and resolves exactly upstream's release lock", () => {
  const vendor = join(ROOT, "vendor", "pi-coding-agent");
  const manifest = json(join(vendor, "manifest.json"));
  const files = record(manifest.files);
  assert.equal(sha256(join(SDK, "package.json")), files["upstream/package.json"], "installed package.json is the pinned upstream file");
  assert.equal(existsSync(join(SDK, "npm-shrinkwrap.json")), false, "the SDK ships no shrinkwrap; its transitive pins come from upstream's install lock");
  assert.deepEqual(manifest.patches, [], "empty patch inventory: the SDK is unpatched");

  const lock = json(join(PACKAGE, "package-lock.json"));
  const locked = record(lock.packages);
  const distribution = record(record(manifest.npm_distribution).packages);
  assert.deepEqual(Object.keys(distribution).sort(), Object.keys(record(record(locked[""]).dependencies)).sort(), "every direct dependency is recorded");

  for (const [name, entry] of Object.entries(distribution)) {
    const pinned = record(entry);
    const installed = record(locked[`node_modules/${name}`]);
    assert.equal(installed.version, pinned.version, name);
    assert.equal(installed.resolved, pinned.resolved, name);
    assert.equal(installed.integrity, pinned.integrity, name);
  }

  for (const [name, version] of Object.entries(record(json(join(SDK, "package.json")).dependencies))) {
    assert.match(text(version), EXACT, `pi pins ${name} exactly`);
    const found = resolveLocked(locked, SDK_PATH, name);
    assert.ok(found, `${name} is locked where the SDK resolves it`);
    assert.equal(record(locked[found]).version, version, name);
    assert.equal(json(text(findPackageJSON(name, pathToFileURL(join(SDK, "dist", "index.js"))))).version, version, `${name} is installed at pi's pin`);
  }

  const upstream = record(json(join(vendor, "upstream", "install-lock", "package-lock.json")).packages);
  assert.equal(record(upstream[""]).version, manifest.version);
  const expected = new Map<string, JsonObject>();
  const keys: string[] = [];

  for (const [path, entry] of Object.entries(upstream)) {
    if (path.length === 0) continue;
    const pinned = record(entry);
    const key = `${nameOf(path)}@${text(pinned.version)}`;
    const seen = expected.get(key);
    assert.ok(seen === undefined || (seen.resolved === pinned.resolved && seen.integrity === pinned.integrity), key);
    expected.set(key, pinned);
    keys.push(key);
  }

  const tree = closure(locked, SDK_PATH);
  const production = Object.keys(locked).filter(path => path.length > 0 && record(locked[path]).dev !== true).sort();
  assert.deepEqual(production, tree, "every production package is in the SDK's dependency closure");
  const actual = tree.map(path => `${nameOf(path)}@${text(record(locked[path]).version)}`);
  assert.deepEqual(actual.sort(), keys.sort(), "the SDK's locked closure is exactly upstream's install lock");

  for (const path of tree) {
    const installed = record(locked[path]);
    const pinned = expected.get(`${nameOf(path)}@${text(installed.version)}`);
    assert.ok(pinned, path);
    assert.equal(installed.resolved, pinned.resolved, path);
    assert.match(text(installed.integrity), /^sha512-/, `${path} is installed with registry integrity`);

    if (pinned.integrity !== undefined) assert.equal(installed.integrity, pinned.integrity, path);
    else assert.ok(nameOf(path).startsWith("@earendil-works/pi-") && installed.version === manifest.version, `${path}: only pi's lockstep packages lack upstream integrity`);
  }
});
