import { Theme } from "@earendil-works/pi-coding-agent";
import type { ExtensionUIContext, ExtensionUIDialogOptions } from "@earendil-works/pi-coding-agent";
import type { Id, UiAnswer, UiCloseReason, UiPrompt, WorkerToService } from "../dto.ts";
import { counter, newId } from "../schema.ts";
import type { Schema } from "../schema.ts";
import type { SystemNotice } from "./activity.ts";
import { prefix } from "./text.ts";
import type { RespondBody } from "./wire.ts";
import workerSchema from "./worker-schema.json" with { type: "json" };

const PROMPTS: readonly Schema[] = workerSchema.$defs.UiPrompt.oneOf;

function promptField(kind: UiPrompt["kind"], field: string): Schema | undefined {
  return PROMPTS.find(prompt => prompt.properties?.kind?.const === kind)?.properties?.[field];
}

/** A dialog bound read from the worker schema, so that a clipped `ui.request` always encodes. */
function bound(value: number | undefined): number {
  if (value === undefined) throw new Error("worker-schema.json: a UiPrompt field has no length bound");

  return value;
}

const MAX_DIALOG_TITLE = Math.min(...PROMPTS.map(prompt => bound(prompt.properties?.title?.maxLength)));

const MAX_CONFIRM_MESSAGE = bound(promptField("confirm", "message")?.maxLength);

const MAX_SELECT_OPTIONS = bound(promptField("select", "options")?.maxItems);

const MAX_SELECT_OPTION = bound(promptField("select", "options")?.items?.maxLength);

const MAX_INPUT_PLACEHOLDER = bound(promptField("input", "placeholder")?.maxLength);

const MAX_EDITOR_PREFILL = bound(promptField("editor", "prefill")?.maxLength);

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

const PLAIN = new Theme(COLORS, BACKGROUNDS, "256color", { name: "lade-plain" });

class UnsupportedUiError extends Error {
  readonly name = "UnsupportedUiError";
  constructor(surface: string) { super(`LADE does not support the extension UI surface "${surface}" (terminal-only); the request fails instead of being answered`); }
}

interface Pending { generation: number; resolve: (answer: UiAnswer | undefined) => void }

export class UiBridge {
  private readonly send: (body: WorkerToService) => void;
  private readonly systemNotice: SystemNotice;
  private readonly pending = new Map<Id, Pending>();
  private dialogOpened: (() => void) | undefined;
  private currentGeneration = 1;
  constructor(send: (body: WorkerToService) => void, systemNotice: SystemNotice) { this.send = send; this.systemNotice = systemNotice; }

  get generation(): number { return this.currentGeneration; }

  /** Starts a new session generation, closing every open dialog as `replaced`. */
  advance(): void {
    this.closePending("replaced");
    this.currentGeneration += 1;
  }

  cancel(): void { this.closePending("aborted"); }

  private closePending(reason: Extract<UiCloseReason, "replaced" | "aborted">): void {
    for (const [id, open] of this.pending) {
      this.pending.delete(id);
      this.send({ kind: "ui.closed", ui_id: id, reason });
      open.resolve(undefined);
    }
  }

  respond(body: RespondBody): string | undefined {
    const open = this.pending.get(body.ui_id);

    if (!open) return "no open dialog with this id (answered, closed or replaced)";

    if (counter(open.generation) !== body.generation) return "the dialog belongs to another session generation";
    this.pending.delete(body.ui_id);
    open.resolve(body.answer);

    return undefined;
  }

  async runUntilDialog(run: () => Promise<void>): Promise<void> {
    const opened = new Promise<void>(resolve => {
      this.dialogOpened = resolve;

      if (this.pending.size > 0) resolve();
    });

    try { await Promise.race([run(), opened]); }
    finally { this.dialogOpened = undefined; }
  }

  private ask(prompt: UiPrompt, options: ExtensionUIDialogOptions | undefined): Promise<UiAnswer | undefined> {
    const id = newId();
    const generation = this.currentGeneration;

    return new Promise(resolve => {
      let timer: NodeJS.Timeout | undefined;

      const finish = (answer: UiAnswer | undefined): void => {
        if (timer) clearTimeout(timer);
        options?.signal?.removeEventListener("abort", onAbort);
        resolve(answer);
      };

      const close = (reason: Extract<UiCloseReason, "timeout" | "aborted">): void => {
        if (!this.pending.delete(id)) return;
        this.send({ kind: "ui.closed", ui_id: id, reason });
        finish(undefined);
      };

      function onAbort(): void { close("aborted"); }

      if (options?.signal?.aborted) return resolve(undefined);

      this.pending.set(id, { generation, resolve: finish });
      options?.signal?.addEventListener("abort", onAbort, { once: true });

      if (options?.timeout !== undefined && options.timeout > 0) timer = setTimeout(() => close("timeout"), options.timeout);
      this.send({ kind: "ui.request", ui_id: id, generation: counter(generation), prompt });
      this.dialogOpened?.();
    });
  }

  context(): ExtensionUIContext {
    const value = (answer: UiAnswer | undefined): string | undefined => answer?.kind === "value" ? answer.value : undefined;

    return {
      select: async (title, options, opts) => {
        if (options.length === 0) throw new Error("LADE: a select dialog needs at least one option");
        let shown = options.slice(0, MAX_SELECT_OPTIONS).map(option => prefix(option, MAX_SELECT_OPTION));

        if (new Set(shown).size !== shown.length) {
          shown = shown.map((option, index) => {
            const suffix = ` [${index + 1}]`;

            return `${prefix(option, MAX_SELECT_OPTION - suffix.length)}${suffix}`;
          });
        }

        const answer = value(await this.ask({ kind: "select", title: prefix(title, MAX_DIALOG_TITLE), options: shown }, opts));
        const index = answer === undefined ? -1 : shown.indexOf(answer);

        return index < 0 ? undefined : options[index];
      },
      confirm: async (title, message, opts) => {
        const answer = await this.ask({ kind: "confirm", title: prefix(title, MAX_DIALOG_TITLE), message: prefix(message, MAX_CONFIRM_MESSAGE) }, opts);

        return answer?.kind === "confirm" && answer.confirmed;
      },
      input: async (title, placeholder, opts) => value(await this.ask({ kind: "input", title: prefix(title, MAX_DIALOG_TITLE), placeholder: prefix(placeholder ?? "", MAX_INPUT_PLACEHOLDER) }, opts)),
      editor: async (title, prefill) => value(await this.ask({ kind: "editor", title: prefix(title, MAX_DIALOG_TITLE), prefill: prefix(prefill ?? "", MAX_EDITOR_PREFILL) }, undefined)),
      notify: (message, type) => { this.systemNotice("Extension notice", message, type === "error"); },
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
