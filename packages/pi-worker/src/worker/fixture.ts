// Deterministic, credential-free, offline `lade-fixture` provider. It implements the SDK's custom
// streaming API (`streamSimple`) and is registered on the ModelRuntime like an extension provider,
// so its responses go through pi's real agent loop and its tool calls run pi's real tools.
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";
import type { Api, AssistantMessage, AssistantMessageEventStream, Context, Model, SimpleStreamOptions, TextContent, ToolCall } from "@earendil-works/pi-ai";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";

export const FIXTURE_PROVIDER = "lade-fixture";

export const FIXTURE_MODEL = "scripted";

export const FIXTURE_API = "lade-fixture-scripted";

export const FIXTURE_FAILURE = "lade-fixture: scripted provider failure";

type ProviderRegistration = Parameters<ModelRuntime["registerProvider"]>[1];

interface Script { text: string; calls: ToolCall[]; fail: boolean; delayMs: number }

interface Directives { writes: Array<{ path: string; content: string }>; fail: boolean; delayMs: number }

const DIRECTIVE = /^\s*lade-fixture:\s*([a-z]+)(?:[ \t]+(.*))?$/;

function unescape(text: string): string {
  return text.replace(/\\(\\|n|t)/g, (_match, code: string) => code === "n" ? "\n" : code === "t" ? "\t" : "\\");
}

function directives(text: string): Directives {
  const found: Directives = { writes: [], fail: false, delayMs: 0 };

  for (const line of text.split("\n")) {
    const match = DIRECTIVE.exec(line.replace(/\r$/, ""));

    if (!match) continue;
    const verb = match[1];
    const rest = match[2] ?? "";

    if (verb === "fail") found.fail = true;
    else if (verb === "delay") found.delayMs = Math.min(2_000, Math.max(0, Number.parseInt(rest, 10) || 0));
    else if (verb === "write") {
      const space = rest.search(/[ \t]/);
      const path = space < 0 ? rest : rest.slice(0, space);

      if (path.length > 0) found.writes.push({ path, content: unescape(space < 0 ? "" : rest.slice(space + 1)) });
    }
  }

  return found;
}

function userText(message: Context["messages"][number]): string {
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

function chunks(text: string): string[] {
  const words = text.split(/(?<= )/);
  const size = Math.max(1, Math.ceil(words.length / 4));
  const out: string[] = [];

  for (let i = 0; i < words.length; i += size) out.push(words.slice(i, i + size).join(""));

  return out;
}

/** Script for one model call, decided only from the visible conversation. */
function plan(context: Context): Script {
  const messages = context.messages;
  let lastUser = -1;

  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i]?.role === "user") { lastUser = i; break; }
  }

  const latest = messages[lastUser];
  const script = directives(latest ? userText(latest) : "");
  const results = messages.slice(lastUser + 1).flatMap(message => message.role === "toolResult" ? [message] : []);
  const users = messages.filter(message => message.role === "user").length;

  if (results.length > 0) {
    const failed = results.filter(result => result.isError).length;

    return {
      text: `lade-fixture: ${results.length - failed} of ${results.length} tool call(s) succeeded. Done.`,
      calls: [], fail: false, delayMs: script.delayMs,
    };
  }

  if (script.fail) return { text: "", calls: [], fail: true, delayMs: script.delayMs };

  if (script.writes.length > 0) {
    return {
      text: `lade-fixture: writing ${script.writes.length} file(s) with the write tool.`,
      calls: script.writes.map((write, n) => ({ type: "toolCall", id: `lade-fixture-${users}-${n + 1}`, name: "write", arguments: { path: write.path, content: write.content } })),
      fail: false, delayMs: script.delayMs,
    };
  }

  return {
    text: `lade-fixture scripted reply (offline fixture model, not a real model). The context holds ${users} user message(s).`,
    calls: [], fail: false, delayMs: script.delayMs,
  };
}

async function run(model: Model<Api>, context: Context, options: SimpleStreamOptions | undefined, stream: AssistantMessageEventStream): Promise<void> {
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
      throw new Error(FIXTURE_FAILURE);
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

function streamFixture(model: Model<Api>, context: Context, options?: SimpleStreamOptions): AssistantMessageEventStream {
  const stream = createAssistantMessageEventStream();
  void run(model, context, options, stream);

  return stream;
}

/** Registration through the SDK's supported custom-provider API; the apiKey is a non-secret placeholder. */
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
