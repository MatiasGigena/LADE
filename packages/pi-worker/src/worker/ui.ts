// Semantic extension UI routed to the LADE client. Every dialog carries the session generation it was
// raised in; an answer for another generation, an unknown dialog or a second answer is refused, and a
// session refresh/replacement closes the open dialogs as "replaced". Nothing is ever answered
// automatically: a dialog stays open until the developer answers, the extension's own timeout or abort
// signal closes it, or the session is replaced. TUI-only surfaces (custom components, editor control,
// themes) fail explicitly or are display-only no-ops; they never fabricate a decision.
import { Theme } from "@earendil-works/pi-coding-agent";
import type { ExtensionUIContext, ExtensionUIDialogOptions } from "@earendil-works/pi-coding-agent";
import type { Id } from "../dto.ts";
import { counter, newId } from "../schema.ts";
import { prefix } from "./activity.ts";
import type { OutboundBody, RespondBody, UiAnswer, UiPrompt } from "./wire.ts";

type Notify = (title: string, text: string, isError: boolean) => void;

type ThemeColors = ConstructorParameters<typeof Theme>[0];

type ThemeBackgrounds = ConstructorParameters<typeof Theme>[1];

const COLORS: ThemeColors = {
  accent: "", border: "", borderAccent: "", borderMuted: "", success: "", error: "", warning: "", muted: "", dim: "", text: "",
  thinkingText: "", userMessageText: "", customMessageText: "", customMessageLabel: "", toolTitle: "", toolOutput: "", mdHeading: "",
  mdLink: "", mdLinkUrl: "", mdCode: "", mdCodeBlock: "", mdCodeBlockBorder: "", mdQuote: "", mdQuoteBorder: "", mdHr: "",
  mdListBullet: "", toolDiffAdded: "", toolDiffRemoved: "", toolDiffContext: "", syntaxComment: "", syntaxKeyword: "",
  syntaxFunction: "", syntaxVariable: "", syntaxString: "", syntaxNumber: "", syntaxType: "", syntaxOperator: "",
  syntaxPunctuation: "", thinkingOff: "", thinkingMinimal: "", thinkingLow: "", thinkingMedium: "", thinkingHigh: "",
  thinkingXhigh: "", bashMode: "",
};

const BACKGROUNDS: ThemeBackgrounds = { selectedBg: "", userMessageBg: "", customMessageBg: "", toolPendingBg: "", toolSuccessBg: "", toolErrorBg: "" };

/** Plain (uncoloured) theme: LADE renders extension text itself, never terminal escapes. */
const PLAIN = new Theme(COLORS, BACKGROUNDS, "256color", { name: "lade-plain" });

export class UnsupportedUiError extends Error {
  constructor(surface: string) { super(`LADE does not support the extension UI surface "${surface}" (terminal-only); the request fails instead of being answered`); }
}

interface Pending { generation: number; resolve: (answer: UiAnswer | undefined) => void }

export class UiBridge {
  private readonly send: (body: OutboundBody) => void;
  private readonly notify: Notify;
  private readonly pending = new Map<Id, Pending>();
  private current = 1;
  constructor(send: (body: OutboundBody) => void, notify: Notify) { this.send = send; this.notify = notify; }

  get generation(): number { return this.current; }

  /** A refresh or replacement starts a new generation; open dialogs of the old one are closed. */
  advance(): number {
    for (const [id, open] of this.pending) {
      this.pending.delete(id);
      this.send({ kind: "ui.closed", ui_id: id, reason: "replaced" });
      open.resolve(undefined);
    }

    this.current += 1;

    return this.current;
  }

  /** The developer's answer. Returns why it was refused, or undefined when it was applied. */
  respond(body: RespondBody): string | undefined {
    const open = this.pending.get(body.ui_id);

    if (!open) return "no open dialog with this id (answered, closed or replaced)";

    if (counter(open.generation) !== body.generation) return "the dialog belongs to another session generation";
    this.pending.delete(body.ui_id);
    open.resolve(body.answer);

    return undefined;
  }

  get open(): number { return this.pending.size; }

  private ask(prompt: UiPrompt, options: ExtensionUIDialogOptions | undefined): Promise<UiAnswer | undefined> {
    const id = newId();
    const generation = this.current;

    return new Promise(resolve => {
      let timer: NodeJS.Timeout | undefined;

      const finish = (answer: UiAnswer | undefined): void => {
        if (timer) clearTimeout(timer);
        options?.signal?.removeEventListener("abort", onAbort);
        resolve(answer);
      };

      const close = (reason: "timeout" | "aborted"): void => {
        if (!this.pending.delete(id)) return;
        this.send({ kind: "ui.closed", ui_id: id, reason });
        finish(undefined);
      };

      function onAbort(): void { close("aborted"); }

      if (options?.signal?.aborted) { resolve(undefined);

 return; }

      this.pending.set(id, { generation, resolve: finish });
      options?.signal?.addEventListener("abort", onAbort, { once: true });

      if (options?.timeout !== undefined && options.timeout > 0) timer = setTimeout(() => close("timeout"), options.timeout);
      this.send({ kind: "ui.request", ui_id: id, generation: counter(generation), prompt });
    });
  }

  /** The `ExtensionUIContext` bound to every session runtime of this worker. */
  context(): ExtensionUIContext {
    const value = (answer: UiAnswer | undefined): string | undefined => answer?.kind === "value" ? answer.value : undefined;

    return {
      select: async (title, options, opts) => {
        // The wire needs at least one choice; an empty list fails instead of being answered.
        if (options.length === 0) throw new Error("LADE: a select dialog needs at least one option");
        const shown = options.slice(0, 64).map(option => prefix(option, 512));
        const answer = value(await this.ask({ kind: "select", title: prefix(title, 512), options: shown }, opts));
        // The developer chose among the shown (possibly clipped) options; return the extension's own.
        const index = answer === undefined ? -1 : shown.indexOf(answer);

        return index < 0 ? undefined : options[index];
      },
      confirm: async (title, message, opts) => {
        const answer = await this.ask({ kind: "confirm", title: prefix(title, 512), message: prefix(message, 4096) }, opts);

        return answer?.kind === "confirm" && answer.confirmed;
      },
      input: async (title, placeholder, opts) => value(await this.ask({ kind: "input", title: prefix(title, 512), placeholder: prefix(placeholder ?? "", 512) }, opts)),
      editor: async (title, prefill) => value(await this.ask({ kind: "editor", title: prefix(title, 512), prefill: prefix(prefill ?? "", 65_536) }, undefined)),
      notify: (message, type) => { this.notify("Extension notice", message, type === "error"); },
      onTerminalInput: () => () => {},
      setStatus: () => {},
      setWorkingMessage: () => {},
      setWorkingVisible: () => {},
      setWorkingIndicator: () => {},
      setHiddenThinkingLabel: () => {},
      setWidget: () => {},
      setFooter: () => {},
      setHeader: () => {},
      setTitle: () => {},
      custom: () => Promise.reject(new UnsupportedUiError("custom")),
      pasteToEditor: () => { throw new UnsupportedUiError("pasteToEditor"); },
      setEditorText: () => { throw new UnsupportedUiError("setEditorText"); },
      getEditorText: () => { throw new UnsupportedUiError("getEditorText"); },
      addAutocompleteProvider: () => {},
      setEditorComponent: () => { throw new UnsupportedUiError("setEditorComponent"); },
      getEditorComponent: () => undefined,
      theme: PLAIN,
      getAllThemes: () => [],
      getTheme: () => undefined,
      setTheme: () => ({ success: false, error: "themes are not available in LADE" }),
      getToolsExpanded: () => false,
      setToolsExpanded: () => {},
    };
  }
}
