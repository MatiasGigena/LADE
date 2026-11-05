import type { SessionEntry } from "@earendil-works/pi-coding-agent";
import type { TreeEntry } from "../dto.ts";
import { clip } from "./text.ts";

export const MAX_TREE_ENTRIES = 1024;

const MAX_TREE_BYTES = 900 * 1024;

const MAX_LABEL = 200;

interface TreePage { entries: TreeEntry[]; truncated: boolean }

function contentText(content: string | ReadonlyArray<{ type: string; text?: string }>): string {
  if (!Array.isArray(content)) return String(content);

  return content.map(part => part.type === "text" ? part.text ?? "" : `[${part.type}]`).join(" ");
}

function describe(entry: SessionEntry): Pick<TreeEntry, "kind" | "label"> {
  switch (entry.type) {
    case "message": {
      const message = entry.message;

      if (message.role === "user") return { kind: "user", label: contentText(message.content) };

      if (message.role === "assistant") return { kind: "assistant", label: contentText(message.content) };

      if (message.role === "toolResult") return { kind: "tool_result", label: `${message.toolName} result` };

      return { kind: "other", label: message.role };
    }

    case "custom_message": return { kind: "custom_message", label: entry.customType };
    case "compaction": return { kind: "compaction", label: "Compaction summary" };
    case "branch_summary": return { kind: "branch_summary", label: "Branch summary" };
    case "model_change": return { kind: "model_change", label: `${entry.provider}/${entry.modelId}` };
    case "thinking_level_change": return { kind: "thinking_change", label: entry.thinkingLevel };
    case "label": return { kind: "label", label: entry.label ?? "label cleared" };
    case "session_info": return { kind: "session_info", label: entry.name ?? "session info" };
    case "custom": return { kind: "custom", label: entry.customType };
    default: return { kind: "other", label: "entry" };
  }
}

export function treeEntry(entry: SessionEntry, bookmark: string | undefined): TreeEntry {
  const { kind, label } = describe(entry);
  const node: TreeEntry = { kind, entry_id: entry.id, label: clip(label, MAX_LABEL) };

  if (entry.parentId !== null) node.parent_id = entry.parentId;

  if (bookmark !== undefined) node.bookmark = clip(bookmark, MAX_LABEL);

  return node;
}

/** The newest entries, oldest first, that fit one `session.result` frame. */
export function treePage(entries: readonly SessionEntry[], label: (id: string) => string | undefined): TreePage {
  const page: TreeEntry[] = [];
  let bytes = 0;

  for (let i = entries.length - 1; i >= 0 && page.length < MAX_TREE_ENTRIES; i--) {
    const entry = entries[i];

    if (entry === undefined) break;
    const node = treeEntry(entry, label(entry.id));
    bytes += Buffer.byteLength(JSON.stringify(node)) + 1;

    if (bytes > MAX_TREE_BYTES) break;
    page.push(node);
  }

  return { entries: page.reverse(), truncated: page.length < entries.length };
}
