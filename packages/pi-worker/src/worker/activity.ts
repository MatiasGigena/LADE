import { isString, MAX_ITEM_TEXT_BYTES, newId } from "../schema.ts";
import type { AgentSessionEvent, AgentToolResult } from "@earendil-works/pi-coding-agent";
import type { ActivityRole, Id, WorkerToService } from "../dto.ts";
import type { ActivityItemBody, TurnState } from "./wire.ts";
import { clip, isHighSurrogate, safeMessage } from "./text.ts";

const MAX_ENCODED_TEXT = 900 * 1024;

const MAX_TITLE = 200;

const MAX_TOOL_TARGET = 120;

type SessionMessage = Extract<AgentSessionEvent, { type: "message_end" }>["message"];

type CompactionEnd = Extract<AgentSessionEvent, { type: "compaction_end" }>;

type StreamEvent = Extract<AgentSessionEvent, { type: "message_update" }>["assistantMessageEvent"];

type ToolOutput = Pick<AgentToolResult<void>, "content">;

/** Reports a worker-side event to the user as a `system` activity item. */
export type SystemNotice = (title: string, text: string, isError: boolean) => void;

interface Bounded { text: string; truncated: boolean; bytes: number }

export function boundText(text: string): Bounded {
  const bytes = Buffer.from(text, "utf8");
  let limit = MAX_ITEM_TEXT_BYTES;

  for (;;) {
    let cut = Math.min(limit, bytes.length);

    while (cut > 0 && cut < bytes.length && ((bytes[cut] ?? 0) & 0xc0) === 0x80) cut--;
    const prefix = bytes.subarray(0, cut).toString("utf8");

    if (cut === 0 || Buffer.byteLength(JSON.stringify(prefix)) <= MAX_ENCODED_TEXT) return { text: prefix, truncated: cut < bytes.length, bytes: bytes.length };
    limit = Math.floor(cut / 2);
  }
}

function messageText(message: SessionMessage): string {
  if (message.role !== "user") return "";

  if (!Array.isArray(message.content)) return message.content;

  return message.content.map(part => part.type === "text" ? part.text : `[${part.type}]`).join("\n");
}

interface Block { itemId: Id; role: ActivityRole; title: string; sent: number; carry: string; capped: boolean }

interface Tool { itemId: Id; name: string; title: string; args: string }

export class ActivityMapper {
  private readonly send: (body: WorkerToService) => void;
  private readonly blocks = new Map<number, Block>();
  private readonly tools = new Map<string, Tool>();
  private turn: TurnState = "idle";
  constructor(send: (body: WorkerToService) => void) { this.send = send; }

  handle(event: AgentSessionEvent): void {
    switch (event.type) {
      case "agent_start": return this.setTurn("running");
      case "agent_settled": return this.setTurn("idle");
      case "message_start":
        if (event.message.role === "assistant") this.blocks.clear();

        return;
      case "message_update": return this.update(event.assistantMessageEvent);
      case "message_end": return this.finish(event.message);
      case "tool_execution_start": {
        // The SDK leaves tool arguments untyped: only a string path or command names the target.
        const path: unknown = event.args?.path;
        const command: unknown = event.args?.command;

        return this.toolStart(event.toolCallId, event.toolName, JSON.stringify(event.args, null, 2) ?? "", isString(path) ? path : isString(command) ? command : undefined);
      }

      case "tool_execution_end": return this.toolEnd(event.toolCallId, event.result, event.isError);
      case "compaction_start": return this.system("Compacting context", `Context compaction started (${event.reason}).`, false);
      case "compaction_end": return this.compactionEnd(event);
      case "auto_retry_start": return this.system("Retrying model request", `Attempt ${event.attempt} of ${event.maxAttempts} after ${event.delayMs} ms (${safeMessage(event.errorMessage)}).`, false);
      case "auto_retry_end": return this.system(event.success ? "Retry succeeded" : "Retry gave up", event.success ? `The model request succeeded on attempt ${event.attempt}.` : `The model request failed after ${event.attempt} attempt(s): ${safeMessage(event.finalError ?? "unknown error")}.`, !event.success);
      default:
    }
  }

  readonly system: SystemNotice = (title, text, isError) => {
    this.item(newId(), "system", title, text, isError);
  };

  private compactionEnd({ reason, aborted, willRetry, errorMessage: error, result }: CompactionEnd): void {
    const tokensBefore = result?.tokensBefore;

    if (aborted) this.system("Compaction aborted", `Context compaction (${reason}) was aborted.`, false);
    else if (error !== undefined) this.system("Compaction failed", `Context compaction (${reason}) failed${willRetry ? " and will be retried" : ""}: ${safeMessage(error)}.`, true);
    else this.system("Context compacted", `Context compaction (${reason}) finished${tokensBefore === undefined ? "" : ` (${tokensBefore} tokens before)`}.`, false);
  }

  private setTurn(state: TurnState): void {
    if (this.turn === state) return;
    this.turn = state;
    this.send({ kind: "turn.state", state });
  }

  private item(itemId: Id, role: ActivityRole, title: string, text: string, isError: boolean, toolName?: string): void {
    const bounded = boundText(text);
    const label = bounded.truncated ? clip(`${title} (truncated; ${bounded.bytes} bytes)`, MAX_TITLE) : clip(title, MAX_TITLE);
    const body: ActivityItemBody = { kind: "activity.item", item_id: itemId, role, title: label, text: bounded.text, is_error: isError, truncated: bounded.truncated };

    if (toolName !== undefined) body.tool_name = clip(toolName, MAX_TITLE);
    this.send(body);
  }

  /** The block streaming content `index`; the first use opens it and announces it as an empty item. */
  private openBlock(index: number, role: ActivityRole): Block {
    const existing = this.blocks.get(index);

    if (existing) return existing;
    const created: Block = { itemId: newId(), role, title: role === "thinking" ? "Thinking" : "Assistant", sent: 0, carry: "", capped: false };
    this.blocks.set(index, created);
    this.item(created.itemId, role, created.title, "", false);

    return created;
  }

  private update(event: StreamEvent): void {
    switch (event.type) {
      case "text_start": return void this.openBlock(event.contentIndex, "assistant");
      case "thinking_start": return void this.openBlock(event.contentIndex, "thinking");
      case "text_delta": return this.delta(this.openBlock(event.contentIndex, "assistant"), event.delta);
      case "thinking_delta": return this.delta(this.openBlock(event.contentIndex, "thinking"), event.delta);
      default:
    }
  }

  private delta(block: Block, delta: string): void {
    let text = block.carry + delta;
    block.carry = "";

    if (text.length > 0 && isHighSurrogate(text.charCodeAt(text.length - 1))) { block.carry = text.slice(-1); text = text.slice(0, -1); }

    if (block.capped || text.length === 0) return;
    const bounded = boundText(text);
    text = bounded.text;
    const size = Buffer.byteLength(text);

    const full = block.sent + size > MAX_ITEM_TEXT_BYTES;
    block.capped = full || bounded.truncated;

    if (full) return;
    block.sent += size;
    this.send({ kind: "activity.delta", item_id: block.itemId, append_text: text });
  }

  private finish(message: SessionMessage): void {
    if (message.role === "user") {
      this.item(newId(), "user", "User", messageText(message), false);

      return;
    }

    if (message.role !== "assistant") return;

    for (const [index, part] of message.content.entries()) {
      if (part.type === "text") this.item(this.blocks.get(index)?.itemId ?? newId(), "assistant", "Assistant", part.text, false);
      else if (part.type === "thinking") this.item(this.blocks.get(index)?.itemId ?? newId(), "thinking", "Thinking", part.redacted ? "[redacted thinking]" : part.thinking, false);
    }

    this.blocks.clear();

    if (message.stopReason === "error") {
      const detail = safeMessage(message.errorMessage ?? "model request failed");
      this.system("Model request failed", detail, true);
      this.send({ kind: "worker.error", code: "provider_failed", message: detail });
    } else if (message.stopReason === "aborted") this.system("Turn aborted", "The model request was aborted.", false);
  }

  private toolStart(id: string, name: string, args: string, target: string | undefined): void {
    const tool: Tool = { itemId: newId(), name, title: target === undefined ? name : `${name} ${clip(target, MAX_TOOL_TARGET)}`, args };
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
