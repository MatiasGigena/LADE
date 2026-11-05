import { createAssistantMessageEventStream, getCurrentSystemPrompt } from "./pi-ai.ts";
import type { Api, AssistantMessage, AssistantMessageEventStream, Message, Model, SimpleStreamOptions, TextContent, ToolCall, TranscriptContext } from "./pi-ai.ts";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { compareCodeUnits } from "../json.ts";

export const FIXTURE_PROVIDER = "lade-fixture";

export const FIXTURE_SWITCH = "LADE_TEST_FIXTURE_PROVIDER";

export const FIXTURE_MODEL = "scripted";

export const FIXTURE_API = "lade-fixture-scripted";

export const FIXTURE_FAILURE = "lade-fixture: scripted provider failure";

export const FIXTURE_TRANSIENT = "lade-fixture: overloaded (scripted transient failure)";

const MARKER = /LADE-MARKER:[A-Za-z0-9_.-]+/g;

type ProviderRegistration = Parameters<ModelRuntime["registerProvider"]>[1];

interface Script { text: string; calls: ToolCall[]; fail: boolean; delayMs: number; transient?: boolean }

interface Directives { writes: Array<{ path: string; content: string }>; fail: boolean; delayMs: number; echo: boolean; system: boolean; transient: boolean }

const DIRECTIVE = /^\s*lade-fixture:\s*([a-z]+)(?:[ \t]+(.*))?$/;

const MAX_DELAY_MS = 2_000;

function unescape(text: string): string {
  return text.replace(/\\(\\|n|t)/g, (_match, code: string) => code === "n" ? "\n" : code === "t" ? "\t" : "\\");
}

function addWrite(found: Directives, rest: string): void {
  const space = rest.search(/[ \t]/);
  const path = space < 0 ? rest : rest.slice(0, space);

  if (path.length > 0) found.writes.push({ path, content: unescape(space < 0 ? "" : rest.slice(space + 1)) });
}

const VERBS = new Map<string, (found: Directives, rest: string) => void>([
  ["fail", found => { found.fail = true; }],
  ["echo", found => { found.echo = true; }],
  ["system", found => { found.system = true; }],
  ["transient", found => { found.transient = true; }],
  ["delay", (found, rest) => { found.delayMs = Math.min(MAX_DELAY_MS, Math.max(0, Number.parseInt(rest, 10) || 0)); }],
  ["write", addWrite],
]);

function directives(text: string): Directives {
  const found: Directives = { writes: [], fail: false, delayMs: 0, echo: false, system: false, transient: false };

  for (const line of text.split("\n")) {
    const match = DIRECTIVE.exec(line.replace(/\r$/, ""));

    if (match) VERBS.get(match[1] ?? "")?.(found, match[2] ?? "");
  }

  return found;
}

function userText(message: Message): string {
  if (message.role !== "user") return "";

  if (Array.isArray(message.content)) return message.content.map(part => part.type === "text" ? part.text : "").join("\n");

  return message.content;
}

function pause(ms: number, signal: AbortSignal | undefined): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new Error("aborted"));

      return;
    }

    const timer = setTimeout(() => { signal?.removeEventListener("abort", onAbort); resolve(); }, ms);

    function onAbort(): void { clearTimeout(timer); reject(new Error("aborted")); }

    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

const transientFailed = new Set<string>();

/** True the first time a transient directive is seen for this user message, so its retry succeeds. */
function firstTransientAttempt(message: Message | undefined): boolean {
  const key = message ? `${message.timestamp}:${userText(message)}` : "";

  if (transientFailed.has(key)) return false;
  transientFailed.add(key);

  return true;
}

function chunks(text: string): string[] {
  const words = text.split(/(?<= )/);
  const size = Math.max(1, Math.ceil(words.length / 4));
  const out: string[] = [];

  for (let i = 0; i < words.length; i += size) out.push(words.slice(i, i + size).join(""));

  return out;
}

type Reply = Omit<Script, "delayMs">;

function textReply(text: string): Reply {
  return { text, calls: [], fail: false };
}

function toolSummary(results: ReadonlyArray<{ isError: boolean }>): Reply {
  const failed = results.filter(result => result.isError).length;

  return textReply(`lade-fixture: ${results.length - failed} of ${results.length} tool call(s) succeeded. Done.`);
}

function systemMarkers(context: TranscriptContext): Reply {
  const markers = [...new Set(getCurrentSystemPrompt(context.messages).match(MARKER) ?? [])].sort(compareCodeUnits);

  return textReply(`lade-fixture system markers: ${markers.length > 0 ? markers.join(" ") : "none"}`);
}

function writeCalls(writes: Directives["writes"], users: number): Reply {
  return {
    text: `lade-fixture: writing ${writes.length} file(s) with the write tool.`,
    calls: writes.map((write, n) => ({ type: "toolCall", id: `lade-fixture-${users}-${n + 1}`, name: "write", arguments: { path: write.path, content: write.content } })),
    fail: false,
  };
}

function reply(context: TranscriptContext, lastUser: number, script: Directives): Reply {
  const messages = context.messages;
  const latest = messages[lastUser];
  const results = messages.slice(lastUser + 1).flatMap(message => message.role === "toolResult" ? [message] : []);
  const users = messages.filter(message => message.role === "user").length;

  if (results.length > 0) return toolSummary(results);

  if (script.fail) return { text: "", calls: [], fail: true };

  if (script.transient && firstTransientAttempt(latest)) return { text: "", calls: [], fail: true, transient: true };

  if (script.echo) return textReply(`lade-fixture echo: ${latest ? userText(latest) : ""}`);

  if (script.system) return systemMarkers(context);

  if (script.writes.length > 0) return writeCalls(script.writes, users);

  return textReply(`lade-fixture scripted reply (offline fixture model, not a real model). The context holds ${users} user message(s).`);
}

function plan(context: TranscriptContext): Script {
  const lastUser = context.messages.findLastIndex(message => message.role === "user");
  const latest = context.messages[lastUser];
  const script = directives(latest ? userText(latest) : "");

  return { ...reply(context, lastUser, script), delayMs: script.delayMs };
}

async function run(model: Model<Api>, context: TranscriptContext, options: SimpleStreamOptions | undefined, stream: AssistantMessageEventStream): Promise<void> {
  const signal = options?.signal;
  const script = plan(context);

  const output: AssistantMessage = {
    role: "assistant",
    content: [],
    api: model.api,
    provider: model.provider,
    model: model.id,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
    stopReason: "pending",
    timestamp: Date.now(),
  };

  try {
    stream.push({ type: "start", partial: output });

    if (script.fail) {
      await pause(script.delayMs, signal);
      throw new Error(script.transient ? FIXTURE_TRANSIENT : FIXTURE_FAILURE);
    }

    const block: TextContent = { type: "text", text: "" };
    output.content.push(block);
    const textIndex = output.content.length - 1;
    stream.push({ type: "text_start", contentIndex: textIndex, partial: output });

    for (const delta of chunks(script.text)) {
      await pause(script.delayMs, signal);
      block.text += delta;
      stream.push({ type: "text_delta", contentIndex: textIndex, delta, partial: output });
    }

    stream.push({ type: "text_end", contentIndex: textIndex, content: block.text, partial: output });

    for (const call of script.calls) {
      await pause(script.delayMs, signal);
      output.content.push(call);
      const index = output.content.length - 1;
      stream.push({ type: "toolcall_start", contentIndex: index, partial: output });
      stream.push({ type: "toolcall_delta", contentIndex: index, delta: JSON.stringify(call.arguments), partial: output });
      stream.push({ type: "toolcall_end", contentIndex: index, toolCall: call, partial: output });
    }

    output.stopReason = script.calls.length > 0 ? "toolUse" : "stop";
    stream.push({ type: "done", reason: output.stopReason, message: output });
  } catch (error) {
    output.stopReason = signal?.aborted ? "aborted" : "error";
    output.errorMessage = signal?.aborted ? "aborted" : error instanceof Error ? error.message : FIXTURE_FAILURE;
    stream.push({ type: "error", reason: output.stopReason, error: output });
  }

  stream.end();
}

function streamFixture(model: Model<Api>, context: TranscriptContext, options?: SimpleStreamOptions): AssistantMessageEventStream {
  const stream = createAssistantMessageEventStream();
  void run(model, context, options, stream);

  return stream;
}

export function fixtureProvider(): ProviderRegistration {
  return {
    name: "LADE fixture provider (offline, scripted; not a real model)",
    baseUrl: "lade-fixture://offline",
    apiKey: "lade-fixture-placeholder-not-a-credential",
    api: FIXTURE_API,
    streamSimple: streamFixture,
    models: [{
      id: FIXTURE_MODEL,
      name: "LADE fixture: scripted (offline test model)",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128_000,
      maxTokens: 4_096,
    }],
  };
}

export function registerTestFixture(runtime: ModelRuntime): boolean {
  if (process.env[FIXTURE_SWITCH] !== "1") return false;
  runtime.registerProvider(FIXTURE_PROVIDER, fixtureProvider());

  return true;
}
