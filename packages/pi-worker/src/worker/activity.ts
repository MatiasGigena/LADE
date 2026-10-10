import { randomUUID } from "node:crypto";
import type { AgentSessionEvent, AgentToolResult } from "@earendil-works/pi-coding-agent";
import type { ActivityItemBody, ActivityRole, OutboundBody, TurnState } from "./wire.ts";
import { MAX_ERROR_MESSAGE } from "./wire.ts";

/** Per-item text bound from delivery plan §4.2/§7. */
export const MAX_ITEM_BYTES = 192 * 1024;

/** JSON escaping can grow text up to 6x; keep every encoded item well below the 1 MiB frame. */
const MAX_ENCODED_TEXT = 900 * 1024;

const MAX_TITLE = 200;

type SessionMessage = Extract<AgentSessionEvent, { type: "message_end" }>["message"];

type StreamEvent = Extract<AgentSessionEvent, { type: "message_update" }>["assistantMessageEvent"];

type ToolOutput = Pick<AgentToolResult<void>, "content">;

interface Bounded { text: string; truncated: boolean; bytes: number }

/** UTF-8 prefix of at most MAX_ITEM_BYTES (lone surrogates normalised) whose JSON fits a frame. */
export function boundText(text: string): Bounded {
  const bytes = Buffer.from(text, "utf8");
  let limit = MAX_ITEM_BYTES;

  for (;;) {
    let cut = Math.min(limit, bytes.length);

    while (cut > 0 && cut < bytes.length && ((bytes[cut] ?? 0) & 0xc0) === 0x80) cut--;
    const prefix = bytes.subarray(0, cut).toString("utf8");

    if (cut === 0 || Buffer.byteLength(JSON.stringify(prefix)) <= MAX_ENCODED_TEXT) return { text: prefix, truncated: cut < bytes.length, bytes: bytes.length };
    limit = Math.floor(cut / 2);
  }
}

/** Diagnostic text for worker.error: one line, likely credentials masked, never longer than the wire cap. */
export function safeMessage(text: string): string {
  const line = text.replace(/\p{Cc}+/gu, " ").replace(/[A-Za-z0-9_\-+/=.]{32,}/g, "[redacted]").trim();

  return line.length > MAX_ERROR_MESSAGE ? `${line.slice(0, MAX_ERROR_MESSAGE - 1)}…` : line;
}

function clip(text: string, max: number): string {
  const line = text.replace(/\s+/g, " ").trim();

  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

function messageText(message: SessionMessage): string {
  if (message.role !== "user") return "";

  if (!Array.isArray(message.content)) return message.content;

  return message.content.map(part => part.type === "text" ? part.text : `[${part.type}]`).join("\n");
}

function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}

interface Block { itemId: string; role: ActivityRole; title: string; sent: number; carry: string; capped: boolean }

interface Tool { itemId: string; name: string; title: string; args: string }

/**
 * Projects SDK session events onto §4.2 activity bodies. Streaming text/thinking is announced as an
 * empty item, extended by deltas keyed by content index, and replaced by the authoritative
 * `message_end` content under the same item_id. Tool executions are one `tool` item each.
 */
export class ActivityMapper {
  private readonly send: (body: OutboundBody) => void;
  private readonly blocks = new Map<number, Block>();
  private readonly tools = new Map<string, Tool>();
  private turn: TurnState = "idle";
  constructor(send: (body: OutboundBody) => void) { this.send = send; }

  handle(event: AgentSessionEvent): void {
    switch (event.type) {
      case "agent_start": this.setTurn("running");

        return;
      case "agent_settled": this.setTurn("idle");

        return;
      case "message_start":
        if (event.message.role === "assistant") this.blocks.clear();

        return;
      case "message_update": this.update(event.assistantMessageEvent);

        return;
      case "message_end": this.finish(event.message);

        return;
      case "tool_execution_start": this.toolStart(event.toolCallId, event.toolName, JSON.stringify(event.args, null, 2) ?? "", event.args?.path, event.args?.command);

        return;
      case "tool_execution_end": this.toolEnd(event.toolCallId, event.result, event.isError);

        return;
      case "compaction_start": this.system("Compacting context", `Context compaction started (${event.reason}).`, false);

        return;
      case "auto_retry_start": this.system("Retrying model request", `Attempt ${event.attempt} of ${event.maxAttempts} after ${event.delayMs} ms.`, false);

        return;
      default:
    }
  }

  /** A worker-originated transcript note (never contains prompt or source text). */
  system(title: string, text: string, isError: boolean): void {
    this.item(randomUUID(), "system", title, text, isError, undefined);
  }

  private setTurn(state: TurnState): void {
    if (this.turn === state) return;
    this.turn = state;
    this.send({ kind: "turn.state", state });
  }

  private item(itemId: string, role: ActivityRole, title: string, text: string, isError: boolean, toolName: string | undefined): void {
    const bounded = boundText(text);
    const label = bounded.truncated ? clip(`${title} (truncated; ${bounded.bytes} bytes)`, MAX_TITLE) : clip(title, MAX_TITLE);
    const body: ActivityItemBody = { kind: "activity.item", item_id: itemId, role, title: label, text: bounded.text, is_error: isError, truncated: bounded.truncated };

    if (toolName !== undefined) body.tool_name = clip(toolName, MAX_TITLE);
    this.send(body);
  }

  private block(index: number, role: ActivityRole): Block {
    const existing = this.blocks.get(index);

    if (existing) return existing;
    const created: Block = { itemId: randomUUID(), role, title: role === "thinking" ? "Thinking" : "Assistant", sent: 0, carry: "", capped: false };
    this.blocks.set(index, created);
    this.item(created.itemId, role, created.title, "", false, undefined);

    return created;
  }

  private update(event: StreamEvent): void {
    switch (event.type) {
      case "text_start": this.block(event.contentIndex, "assistant");

        return;
      case "thinking_start": this.block(event.contentIndex, "thinking");

        return;
      case "text_delta": this.delta(this.block(event.contentIndex, "assistant"), event.delta);

        return;
      case "thinking_delta": this.delta(this.block(event.contentIndex, "thinking"), event.delta);

        return;
      default:
    }
  }

  private delta(block: Block, delta: string): void {
    let text = block.carry + delta;
    block.carry = "";

    // Never split a surrogate pair across deltas; hold the high half for the next chunk.
    if (text.length > 0 && isHighSurrogate(text.charCodeAt(text.length - 1))) { block.carry = text.slice(-1); text = text.slice(0, -1); }

    if (block.capped || text.length === 0) return;
    const size = Buffer.byteLength(text);

    // The final message_end item carries the truncated flag; stop streaming at the item bound.
    if (block.sent + size > MAX_ITEM_BYTES) { block.capped = true;

 return; }

    block.sent += size;
    this.send({ kind: "activity.delta", item_id: block.itemId, append_text: text });
  }

  private finish(message: SessionMessage): void {
    if (message.role === "user") {
      this.item(randomUUID(), "user", "User", messageText(message), false, undefined);

      return;
    }

    if (message.role !== "assistant") return;

    for (const [index, part] of message.content.entries()) {
      if (part.type === "text") this.item(this.blocks.get(index)?.itemId ?? randomUUID(), "assistant", "Assistant", part.text, false, undefined);
      else if (part.type === "thinking") this.item(this.blocks.get(index)?.itemId ?? randomUUID(), "thinking", "Thinking", part.redacted ? "[redacted thinking]" : part.thinking, false, undefined);
    }

    this.blocks.clear();

    if (message.stopReason === "error") {
      const detail = safeMessage(message.errorMessage ?? "model request failed");
      this.system("Model request failed", detail, true);
      this.send({ kind: "worker.error", code: "provider_failed", message: detail });
    } else if (message.stopReason === "aborted") this.system("Turn aborted", "The model request was aborted.", false);
  }

  private toolStart(id: string, name: string, args: string, path: string | undefined, command: string | undefined): void {
    const target = path ?? command;
    const tool: Tool = { itemId: randomUUID(), name, title: target === undefined ? name : `${name} ${clip(String(target), 120)}`, args };
    this.tools.set(id, tool);
    this.item(tool.itemId, "tool", tool.title, `arguments:\n${args}`, false, name);
  }

  private toolEnd(id: string, result: ToolOutput | undefined, isError: boolean): void {
    const tool = this.tools.get(id);

    if (!tool) return;
    this.tools.delete(id);
    const parts = Array.isArray(result?.content) ? result.content : [];
    const output = parts.map(part => part.type === "text" ? part.text : `[${part.type}]`).join("\n");
    this.item(tool.itemId, "tool", tool.title, `arguments:\n${tool.args}\n\nresult:\n${output}`, isError, tool.name);
  }
}
