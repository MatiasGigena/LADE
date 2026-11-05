import { existsSync, mkdirSync } from "node:fs";
import { dirname, isAbsolute, join, relative } from "node:path";
import { createAgentSessionFromServices, createAgentSessionRuntime, ModelRuntime, SessionManager, SettingsManager } from "@earendil-works/pi-coding-agent";
import type { AgentSession, AgentSessionRuntime, AgentSessionServices, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeResult, ExtensionFactory, SessionEntry } from "@earendil-works/pi-coding-agent";
import type { Model, ThinkingLevel, TreeEntry, WorkerErrorCode, WorkerResources, WorkerToService } from "../dto.ts";
import { counter } from "../schema.ts";
import { prefix, safeMessage } from "./activity.ts";
import { registerTestFixture } from "./fixture.ts";
import { LadeResourceLoader } from "./resources.ts";
import type { UiBridge } from "./ui.ts";
import type { CommandBody, DeliverBody, ResultBody, StartBody } from "./wire.ts";

export const WORKER_TOOLS = ["read", "bash", "edit", "write"];

export const MAX_TREE_ENTRIES = 1024;

const MAX_TREE_BYTES = 900 * 1024;

const MAX_LABEL = 200;

type Bindings = Parameters<AgentSession["bindExtensions"]>[0];

type RuntimeOptions = Parameters<CreateAgentSessionRuntimeFactory>[0];

type ChangeReason = Extract<WorkerToService, { kind: "session.changed" }>["reason"];

export class StartError extends Error {
  readonly code: WorkerErrorCode;
  constructor(code: WorkerErrorCode, message: string) { super(message); this.code = code; }
}

class CommandError extends Error {}

export interface ProfilePaths { agentDir: string; sessionsDir: string }

export function profilePaths(sessionDir: string): ProfilePaths {
  return { agentDir: join(sessionDir, "agent"), sessionsDir: join(sessionDir, "sessions") };
}

export function piEnvironment(sessionDir: string): ReadonlyArray<readonly [string, string]> {
  const { agentDir, sessionsDir } = profilePaths(sessionDir);

  return [["PI_CODING_AGENT_DIR", agentDir], ["PI_CODING_AGENT_SESSION_DIR", sessionsDir], ["PI_OFFLINE", "1"], ["PI_SKIP_VERSION_CHECK", "1"], ["PI_TELEMETRY", "0"]];
}

function inside(parent: string, path: string): boolean {
  const rel = relative(parent, path);

  return rel.length > 0 && !rel.startsWith("..") && !isAbsolute(rel);
}

function clip(text: string): string {
  const line = text.replace(/\s+/g, " ").trim();

  return line.length > MAX_LABEL ? `${prefix(line, MAX_LABEL - 1)}…` : line;
}

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
  const node: TreeEntry = { kind, entry_id: entry.id, label: clip(label) };

  if (entry.parentId !== null) node.parent_id = entry.parentId;

  if (bookmark !== undefined) node.bookmark = clip(bookmark);

  return node;
}

export interface TreePage { entries: TreeEntry[]; truncated: boolean }

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

function isThinking(level: string): level is ThinkingLevel {
  return ["off", "minimal", "low", "medium", "high", "xhigh", "max"].includes(level);
}

export class InputGate {
  private active: (() => boolean) | undefined;

  readonly extension: ExtensionFactory = pi => {
    pi.on("input", event => event.source === "rpc" && this.active?.() === false ? { action: "handled" } : { action: "continue" });
  };

  open(active: () => boolean): () => void {
    this.active = active;

    return () => { if (this.active === active) this.active = undefined; };
  }
}

export class SessionHost {
  readonly gate = new InputGate();
  private readonly start: StartBody;
  private readonly send: (body: WorkerToService) => void;
  private readonly ui: UiBridge;
  private readonly observe: (session: AgentSession) => () => void;
  private readonly note: (title: string, text: string, isError: boolean) => void;
  private runtimeHost: AgentSessionRuntime | undefined;
  private loader: LadeResourceLoader | undefined;
  private resources: WorkerResources | undefined;
  private model: Model;
  private thinking: ThinkingLevel | undefined;
  private reason: ChangeReason = "start";
  private unsubscribe: (() => void) | undefined;
  private busy = false;
  private announced = false;

  constructor(start: StartBody, send: (body: WorkerToService) => void, ui: UiBridge, observe: (session: AgentSession) => () => void, note: (title: string, text: string, isError: boolean) => void) {
    this.start = start; this.send = send; this.ui = ui; this.observe = observe; this.note = note;
    this.resources = start.resources; this.model = start.model; this.thinking = start.thinking;
  }

  get session(): AgentSession {
    if (!this.runtimeHost) throw new StartError("internal", "session runtime is not open");

    return this.runtimeHost.session;
  }

  async open(): Promise<void> {
    const start = this.start;

    if (!isAbsolute(start.session_dir) || !isAbsolute(start.cwd)) throw new StartError("startup_failed", "session_dir and cwd must be absolute paths");
    const mismatch = piEnvironment(start.session_dir).find(([name, value]) => process.env[name] !== value);

    if (mismatch !== undefined) throw new StartError("startup_failed", `the worker environment must set ${mismatch[0]} for this session_dir`);
    const { agentDir, sessionsDir } = profilePaths(start.session_dir);
    mkdirSync(agentDir, { recursive: true, mode: 0o700 });
    mkdirSync(sessionsDir, { recursive: true, mode: 0o700 });
    const profile = start.resources?.profile_dir;

    if (profile !== undefined && !inside(start.session_dir, profile)) throw new StartError("startup_failed", "the profile snapshot must be inside session_dir");
    const models = profile === undefined ? null : join(profile, "models.json");

    const modelRuntime = await ModelRuntime.create({
      authPath: join(agentDir, "auth.json"),
      modelsPath: models !== null && existsSync(models) ? models : null,
      modelsStorePath: join(agentDir, "models-store.json"),
      allowModelNetwork: false,
    });

    registerTestFixture(modelRuntime);

    if (!modelRuntime.getModel(start.model.provider, start.model.id)) throw new StartError("startup_failed", "the requested model is not registered");

    const settingsManager = SettingsManager.inMemory({
      defaultProvider: start.model.provider,
      defaultModel: start.model.id,
      quietStartup: true,
      enableInstallTelemetry: false,
      enableAnalytics: false,
      defaultProjectTrust: "never",
      steeringMode: "one-at-a-time",
      followUpMode: "one-at-a-time",
    });

    const createRuntime: CreateAgentSessionRuntimeFactory = options => this.createSession(modelRuntime, settingsManager, options);
    const sessionManager = this.openManager(sessionsDir);
    this.runtimeHost = await createAgentSessionRuntime(createRuntime, { cwd: start.cwd, agentDir, sessionManager });
    this.runtimeHost.setBeforeSessionInvalidate(() => { this.ui.advance(); });
    this.runtimeHost.setRebindSession(session => this.bind(session));
    await this.bind(this.runtimeHost.session);
  }

  private async createSession(modelRuntime: ModelRuntime, settingsManager: SettingsManager, { agentDir, sessionManager, sessionStartEvent }: RuntimeOptions): Promise<CreateAgentSessionRuntimeResult> {
    const cwd = this.start.cwd;
    const loader = new LadeResourceLoader({ cwd, agentDir, settingsManager, systemNote: this.start.system_note, resources: this.resources, inputGate: this.gate.extension });
    await loader.reload();
    this.loader = loader;
    const services: AgentSessionServices = { cwd, agentDir, modelRuntime, settingsManager, resourceLoader: loader, diagnostics: [] };
    const model = modelRuntime.getModel(this.model.provider, this.model.id);

    if (!model) throw new StartError("startup_failed", "the requested model is not registered");
    const options: Parameters<typeof createAgentSessionFromServices>[0] = { services, sessionManager, model, tools: WORKER_TOOLS };

    if (sessionStartEvent) options.sessionStartEvent = sessionStartEvent;

    if (this.thinking !== undefined) options.thinkingLevel = this.thinking;
    const result = await createAgentSessionFromServices(options);

    return { ...result, services, diagnostics: [] };
  }

  private openManager(sessionsDir: string): SessionManager {
    const start = this.start;
    const file = start.session_file;

    if (file !== undefined && inside(sessionsDir, file) && existsSync(file)) return SessionManager.open(file, sessionsDir, start.cwd);

    return start.resume ? SessionManager.continueRecent(start.cwd, sessionsDir) : SessionManager.create(start.cwd, sessionsDir);
  }

  /** An extension action that LADE refuses: it notes the attempt and rejects. */
  private refusal(what: string): () => Promise<never> {
    return () => {
      this.note("Extension request refused", `An extension asked to ${what}; LADE replaces sessions only through its own session controls.`, true);

      return Promise.reject(new Error(`LADE: extensions cannot ${what}`));
    };
  }

  private bindings(): Bindings {
    return {
      uiContext: this.ui.context(),
      mode: "rpc",
      commandContextActions: {
        waitForIdle: () => this.session.waitForIdle(),
        newSession: this.refusal("start a new session"),
        fork: this.refusal("fork the session"),
        switchSession: this.refusal("switch sessions"),
        navigateTree: async (targetId, options) => {
          const result = await this.session.navigateTree(targetId, { summarize: options?.summarize ?? false });
          this.changed("navigate");

          return { cancelled: result.cancelled };
        },
        reload: async () => { await this.reload(this.resources); },
      },
      shutdownHandler: () => { this.note("Extension request ignored", "An extension asked pi to exit; the Objective's execution is stopped only from LADE.", false); },
      onError: error => { this.note("Extension error", `${error.event}: ${safeMessage(error.error)}`, true); },
    };
  }

  private async bind(session: AgentSession): Promise<void> {
    this.unsubscribe?.();
    this.unsubscribe = this.observe(session);
    await session.bindExtensions(this.bindings());
    const model = session.model;

    if (model && (model.provider !== this.model.provider || model.id !== this.model.id)) {
      const wanted = this.runtimeHost?.services.modelRuntime.getModel(this.model.provider, this.model.id);

      if (wanted) await session.setModel(wanted);
    }

    if (this.thinking !== undefined && session.thinkingLevel !== this.thinking) session.setThinkingLevel(this.thinking);
    this.changed(this.reason);
    this.reason = "start";
  }

  announce(): void {
    this.announced = true;
    this.changed("start");
  }

  private changed(reason: ChangeReason): void {
    if (!this.announced) return;
    const session = this.session;

    const body: Extract<WorkerToService, { kind: "session.changed" }> = {
      kind: "session.changed",
      generation: counter(this.ui.generation),
      reason,
      pi_session_id: session.sessionId,
      session_file: session.sessionFile ?? join(this.start.session_dir, "sessions"),
      model: { provider: session.model?.provider ?? this.model.provider, id: session.model?.id ?? this.model.id },
      thinking: isThinking(session.thinkingLevel) ? session.thinkingLevel : "off",
    };

    const leaf = session.sessionManager.getLeafId();

    if (leaf !== null) body.leaf_id = leaf;
    this.send(body);
    this.send({ kind: "resources.loaded", generation: counter(this.ui.generation), resources: this.loader?.report() ?? [] });
  }

  private async reload(resources: WorkerResources | undefined): Promise<void> {
    this.resources = resources;
    this.loader?.setResources(resources);
    this.ui.advance();
    await this.session.reload();
    this.changed("reload");
  }

  async command(body: CommandBody): Promise<ResultBody> {
    const result: ResultBody = { kind: "session.result", command_id: body.command_id, ok: true, message: "" };

    if (this.busy) return { ...result, ok: false, message: "another session command is in progress" };
    this.busy = true;

    try {
      await this.execute(body, result);
    } catch (error) {
      result.ok = false;
      result.message = safeMessage(error instanceof Error ? error.message : "session command failed");
    } finally {
      this.busy = false;
    }

    return result;
  }

  private idle(what: string): void {
    if (this.session.isStreaming || this.session.isCompacting) throw new CommandError(`${what} needs an idle session; stop or wait for the current turn`);
  }

  private async execute(body: CommandBody, result: ResultBody): Promise<void> {
    const command = body.command;
    const runtime = this.runtimeHost;

    if (!runtime) throw new CommandError("session runtime is not open");
    const session = runtime.session;

    switch (command.op) {
      case "tree":
        this.tree(session, result);

        return;
      case "navigate":
        return this.navigate(session, command.entry_id);
      case "fork":
        this.idle("forking");

        return this.replaceSession("fork", () => runtime.fork(command.entry_id, { position: command.position }));
      case "import":
        this.idle("importing");

        if (!inside(join(this.start.session_dir, "imports"), command.path)) throw new CommandError("imports are read only from the service's import directory");

        return this.replaceSession("import", () => runtime.importFromJsonl(command.path, this.start.cwd));
      case "export":
        if (!inside(join(this.start.session_dir, "exports"), command.path)) throw new CommandError("exports are written only to the service's export directory");
        mkdirSync(dirname(command.path), { recursive: true, mode: 0o700 });
        session.exportToJsonl(command.path);

        return;
      case "compact":
        this.idle("compaction");
        await session.compact(command.instructions);
        this.changed("compact");

        return;
      case "reload":
        this.idle("reload");

        if (!inside(this.start.session_dir, command.resources.profile_dir)) throw new CommandError("the profile snapshot must be inside session_dir");

        return this.reload(command.resources);
      case "configure":
        return this.configure(runtime, command.model, command.thinking);
      default: {
        const unhandled: never = command;

        return unhandled;
      }
    }
  }

  private tree(session: AgentSession, result: ResultBody): void {
    const manager = session.sessionManager;
    const page = treePage(manager.getEntries(), id => manager.getLabel(id));
    result.entries = page.entries;
    result.truncated = page.truncated;
    const leaf = manager.getLeafId();

    if (leaf !== null) result.leaf_id = leaf;
  }

  private async navigate(session: AgentSession, entryId: string): Promise<void> {
    this.idle("tree navigation");

    if (!session.sessionManager.getEntry(entryId)) throw new CommandError("no such session entry");
    const outcome = await session.navigateTree(entryId, { summarize: false });

    if (outcome.cancelled) throw new CommandError("tree navigation was cancelled by an extension");
    this.changed("navigate");
  }

  /** Replaces the session; the rebind it triggers reports `reason` unless an extension cancels it. */
  private async replaceSession(reason: "fork" | "import", replace: () => Promise<{ cancelled: boolean }>): Promise<void> {
    this.reason = reason;
    const outcome = await replace();

    if (!outcome.cancelled) return;
    this.reason = "start";
    throw new CommandError(`${reason} was cancelled by an extension`);
  }

  private async configure(runtime: AgentSessionRuntime, model: Model, thinking: ThinkingLevel): Promise<void> {
    this.idle("changing the model");
    const session = runtime.session;
    const registered = runtime.services.modelRuntime.getModel(model.provider, model.id);

    if (!registered) throw new CommandError("the requested model is not registered");
    await session.setModel(registered);
    this.model = model;
    session.setThinkingLevel(thinking);
    this.thinking = thinking;
    this.changed("configure");
  }

  async dispose(): Promise<void> {
    this.unsubscribe?.();
    await this.runtimeHost?.dispose();
  }
}

export async function deliver(session: AgentSession, gate: InputGate, input: DeliverBody, onRunError: (error: Error) => void, active: () => boolean = () => true): Promise<void> {
  if (session.isStreaming) {
    await (input.mode === "steer" ? session.steer(input.text) : session.followUp(input.text));

    return;
  }

  const space = input.text.indexOf(" ");
  const name = input.text.startsWith("/") ? input.text.slice(1, space < 0 ? undefined : space) : undefined;

  if (name !== undefined && session.extensionRunner.getCommand(name)) {
    session.prompt(input.text, { expandPromptTemplates: true, source: "rpc" }).catch((error: Error) => onRunError(error));

    return;
  }

  await new Promise<void>((resolve, reject) => {
    let accepted = false;
    const close = gate.open(active);

    session.prompt(input.text, {
      expandPromptTemplates: true,
      source: "rpc",
      preflightResult: () => {
        close();
        accepted = true;
        resolve();
      },
    }).then(() => { close(); resolve(); }, (error: Error) => {
      close();

      if (accepted) onRunError(error);
      else reject(error);
    });
  });
}

export async function abort(session: AgentSession, onCleared: (steering: number, followUp: number) => void): Promise<void> {
  const cleared = session.clearQueue();

  if (cleared.steering.length + cleared.followUp.length > 0) onCleared(cleared.steering.length, cleared.followUp.length);
  await session.abort();
}
