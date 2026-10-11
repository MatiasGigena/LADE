// IMP-008 dependency seam: the worker runs the pinned SDK distribution with exactly one pi-ai, the
// copy nested under the shrinkwrapped pi-coding-agent, and the fixture provider streams through that
// same module instance. No network, no package scripts; disposable directories only.
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
import { createAssistantMessageEventStream } from "../src/worker/pi-ai.ts";
import type { Context } from "../src/worker/pi-ai.ts";
import { record, text, WORKER } from "./worker-support.ts";

const PACKAGE = join(dirname(WORKER), "..");

const ROOT = join(PACKAGE, "..", "..");

const SDK = join(PACKAGE, "node_modules", "@earendil-works", "pi-coding-agent");

const NESTED = join(SDK, "node_modules", "@earendil-works");

function json(path: string): JsonObject {
  return record(JSON.parse(readFileSync(path, "utf8")));
}

function sha256(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

test("the worker installs exactly one pi-ai: the copy the shrinkwrapped SDK resolves", () => {
  assert.equal(record(json(join(PACKAGE, "package.json")).dependencies)["@earendil-works/pi-ai"], undefined, "no direct pi-ai dependency");
  const locked = Object.keys(record(json(join(PACKAGE, "package-lock.json")).packages)).filter(path => path.endsWith("node_modules/@earendil-works/pi-ai"));
  assert.deepEqual(locked, ["node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai"]);
  assert.equal(existsSync(join(PACKAGE, "node_modules", "@earendil-works", "pi-ai")), false, "no hoisted second copy is installed");

  const nested = join(NESTED, "pi-ai", "package.json");
  // Bare-specifier resolution from the SDK's own modules (pi-coding-agent and pi-agent-core).
  assert.equal(findPackageJSON("@earendil-works/pi-ai", pathToFileURL(join(SDK, "dist", "index.js"))), nested);
  assert.equal(findPackageJSON("@earendil-works/pi-ai", pathToFileURL(join(NESTED, "pi-agent-core", "dist", "index.js"))), nested);
  // The worker's re-export module points at that package's "." export target.
  const shim = readFileSync(join(PACKAGE, "src", "worker", "pi-ai.ts"), "utf8");
  const targets = [...new Set([...shim.matchAll(/from "([^"]+)"/g)].map(match => match[1] ?? ""))];
  assert.equal(targets.length, 1);
  const target = join(PACKAGE, "src", "worker", targets[0] ?? "");
  assert.equal(findPackageJSON(pathToFileURL(target)), nested);
  assert.equal(join(NESTED, "pi-ai", text(record(record(json(nested).exports)["."]).import)), target);
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
    const context: Context = { messages: [{ role: "user", content: "hello", timestamp: Date.now() }] };

    // The SDK wraps every provider stream in pi-ai's lazyStream; a class from another pi-ai copy
    // would be a different constructor. The fixture's own stream must be the same class.
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

test("the installed SDK is the pinned narrow-fork distribution and follows its shrinkwrap exactly", () => {
  const vendor = join(ROOT, "vendor", "pi-coding-agent");
  const manifest = json(join(vendor, "manifest.json"));
  const files = record(manifest.files);
  assert.equal(sha256(join(SDK, "package.json")), files["upstream/package.json"], "installed package.json is the pinned upstream file");
  assert.equal(sha256(join(SDK, "npm-shrinkwrap.json")), files["upstream/npm-shrinkwrap.json"], "installed shrinkwrap is the pinned upstream file");
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

  const prefix = "node_modules/@earendil-works/pi-coding-agent/";
  const shrinkwrap = record(json(join(vendor, "upstream", "npm-shrinkwrap.json")).packages);
  const nested = Object.keys(locked).filter(path => path.startsWith(prefix)).map(path => path.slice(prefix.length));
  assert.deepEqual(nested.sort(), Object.keys(shrinkwrap).filter(path => path !== "").sort(), "the nested tree is exactly the shrinkwrap");

  for (const path of nested) {
    const upstream = record(shrinkwrap[path]);
    const installed = record(locked[prefix + path]);
    assert.equal(installed.version, upstream.version, path);
    assert.equal(installed.integrity, upstream.integrity, path);
  }
});
