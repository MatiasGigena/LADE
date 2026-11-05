import { existsSync, mkdirSync } from "node:fs";
import { dirname, isAbsolute, join } from "node:path";
import { createAgentSessionFromServices, createAgentSessionRuntime, ModelRuntime, SessionManager, SettingsManager } from "@earendil-works/pi-coding-agent";
import type { AgentSession, AgentSessionRuntime, AgentSessionServices, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeResult, ExtensionFactory } from "@earendil-works/pi-coding-agent";
import type { Model, SessionChangeReason, SessionCommand, ThinkingLevel, WorkerErrorCode, WorkerResources, WorkerToService } from "../dto.ts";
import { counter } from "../schema.ts";
import type { ActivityMapper } from "./activity.ts";
import { registerTestFixture } from "./fixture.ts";
import { isInside, piEnvironment, profilePaths } from "./paths.ts";
import { LadeResourceLoader } from "./resources.ts";
import { safeMessage } from "./text.ts";
import { treePage } from "./tree.ts";
import type { UiBridge } from "./ui.ts";
import { workerError } from "./wire.ts";
import type { ChangedBody, CommandBody, DeliverBody, ResultBody, StartBody } from "./wire.ts";

const WORKER_TOOLS = ["read", "bash", "edit", "write"];

// Every thinking level, checked against the generated union: a new level fails type-checking until it is listed.
const THINKING_LEVELS = { off: true, minimal: true, low: true, medium: true, high: true, xhigh: true, max: true } as const satisfies Record<ThinkingLevel, true>;

type Bindings = Parameters<AgentSession["bindExtensions"]>[0];

type RuntimeOptions = Parameters<CreateAgentSessionRuntimeFactory>[0];

/** The fields a command adds to its `session.result`. */
type CommandOutput = Pick<ResultBody, "entries" | "truncated" | "leaf_id">;

/** A failure to open the session, reported to the service with its code and message. */
export class StartError extends Error {
  readonly name = "StartError";
  readonly code: WorkerErrorCode;
  constructor(code: WorkerErrorCode, message: string) { super(message); this.code = code; }
}

function isThinking(level: string): level is ThinkingLevel {
  return Object.hasOwn(THINKING_LEVELS, level);
}

/** The worker's own `input` handler: it drops an `rpc` input whose delivery was cancelled before the run started. */
class InputGate {
  private active: (() => boolean) | undefined;

  readonly extension: ExtensionFactory = pi => {
    pi.on("input", event => event.source === "rpc" && this.active?.() === false ? { action: "handled" } : { action: "continue" });
  };

  open(active: () => boolean): () => void {
    this.active = active;

    return () => { if (this.active === active) this.active = undefined; };
  }
}

/**
 * Owns the pi session: opening it, input delivery (in order, cancelled by abort), abort, and the session commands
 * together with their admission rules.
 */
export class SessionHost {
  private readonly gate = new InputGate();
  private readonly start: StartBody;
  private readonly send: (body: WorkerToService) => void;
  private readonly ui: UiBridge;
  private readonly mapper: ActivityMapper;
  private runtimeHost: AgentSessionRuntime | undefined;
  private loader: LadeResourceLoader | undefined;
  private resources: WorkerResources | undefined;
  private model: Model;
  private thinking: ThinkingLevel | undefined;
  private reason: SessionChangeReason = "start";
  private unsubscribe: (() => void) | undefined;
  private announced = false;
  private commandRunning = false;
  private delivery = Promise.resolve();
  private delivering = 0;
  private inputEpoch = 0;

  constructor(start: StartBody, send: (body: WorkerToService) => void, ui: UiBridge, mapper: ActivityMapper) {
    this.start = start; this.send = send; this.ui = ui; this.mapper = mapper;
    this.resources = start.resources; this.model = start.model; this.thinking = start.thinking;
  }

  private get runtime(): AgentSessionRuntime {
    if (!this.runtimeHost) throw new Error("session runtime is not open");

    return this.runtimeHost;
  }

  private get session(): AgentSession {
    return this.runtime.session;
  }

  /** pi's session file. A persisted session always has one; the sessions directory stands in for it otherwise. */
  private get sessionFile(): string {
    return this.session.sessionFile ?? profilePaths(this.start.session_dir).sessionsDir;
  }

  async open(): Promise<void> {
    const start = this.start;

    if (!isAbsolute(start.session_dir) || !isAbsolute(start.cwd)) throw new StartError("startup_failed", "session_dir and cwd must be absolute paths");
    const mismatch = piEnvironment(start.session_dir).find(([name, value]) => process.env[name] !== value);

    if (mismatch !== undefined) throw new StartError("startup_failed", `the worker environment must set ${mismatch[0]} for this session_dir`);
    const { agentDir, sessionsDir } = profilePaths(start.session_dir);
    const profile = start.resources?.profile_dir;

    if (profile !== undefined && !isInside(start.session_dir, profile)) throw new StartError("startup_failed", "the profile snapshot must be inside session_dir");

    if (start.session_file !== undefined && !isInside(sessionsDir, start.session_file)) throw new StartError("startup_failed", "session_file must be inside session_dir/sessions");
    mkdirSync(agentDir, { recursive: true, mode: 0o700 });
    mkdirSync(sessionsDir, { recursive: true, mode: 0o700 });
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

    if (!model) throw new Error("the requested model is not registered");
    const options: Parameters<typeof createAgentSessionFromServices>[0] = { services, sessionManager, model, tools: WORKER_TOOLS };

    if (sessionStartEvent) options.sessionStartEvent = sessionStartEvent;

    if (this.thinking !== undefined) options.thinkingLevel = this.thinking;
    const result = await createAgentSessionFromServices(options);

    return { ...result, services, diagnostics: [] };
  }

  /** Opens `session_file` when it exists yet (pi delays the first write), otherwise resumes the newest session or creates one. */
  private openManager(sessionsDir: string): SessionManager {
    const start = this.start;
    const file = start.session_file;

    if (file !== undefined && existsSync(file)) return SessionManager.open(file, sessionsDir, start.cwd);

    return start.resume ? SessionManager.continueRecent(start.cwd, sessionsDir) : SessionManager.create(start.cwd, sessionsDir);
  }

  /** An extension action that LADE refuses: it notes the attempt and rejects. */
  private refusal(what: string): () => Promise<never> {
    return () => {
      this.mapper.system("Extension request refused", `An extension asked to ${what}; LADE replaces sessions only through its own session controls.`, true);

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
          const result = await this.session.navigateTree(targetId, options);

          if (!result.cancelled) this.announceChange("navigate");

          return { cancelled: result.cancelled };
        },
        reload: async () => { await this.reload(this.resources); },
      },
      shutdownHandler: () => { this.mapper.system("Extension request ignored", "An extension asked pi to exit; the Objective's execution is stopped only from LADE.", false); },
      onError: error => { this.mapper.system("Extension error", `${error.event}: ${safeMessage(error.error)}`, true); },
    };
  }

  private async bind(session: AgentSession): Promise<void> {
    this.unsubscribe?.();
    this.unsubscribe = session.subscribe(event => this.mapper.handle(event));
    await session.bindExtensions(this.bindings());
    const model = session.model;

    if (model && (model.provider !== this.model.provider || model.id !== this.model.id)) {
      const wanted = this.runtimeHost?.services.modelRuntime.getModel(this.model.provider, this.model.id);

      if (wanted) await session.setModel(wanted);
    }

    if (this.thinking !== undefined && session.thinkingLevel !== this.thinking) session.setThinkingLevel(this.thinking);
    this.announceChange(this.reason);
    this.reason = "start";
  }

  /** Sends `worker.ready`, then the first `session.changed` and `resources.loaded`. */
  announce(): void {
    this.send({ kind: "worker.ready", pi_session_id: this.session.sessionId, session_file: this.sessionFile });
    this.announced = true;
    this.announceChange("start");
  }

  private announceChange(reason: SessionChangeReason): void {
    if (!this.announced) return;
    const session = this.session;

    const body: ChangedBody = {
      kind: "session.changed",
      generation: counter(this.ui.generation),
      reason,
      pi_session_id: session.sessionId,
      session_file: this.sessionFile,
      model: { provider: session.model?.provider ?? this.model.provider, id: session.model?.id ?? this.model.id },
      thinking: isThinking(session.thinkingLevel) ? session.thinkingLevel : "off",
    };

    const leaf = session.sessionManager.getLeafId();

    if (leaf !== null) body.leaf_id = leaf;
    this.send(body);
    this.send({ kind: "resources.loaded", generation: counter(this.ui.generation), resources: this.loader?.report() ?? [] });
  }

  /** Cancels every input not yet accepted and closes the dialogs they opened. */
  cancelInputs(): void {
    this.inputEpoch++;
    this.ui.cancel();
  }

  /** Acknowledges an input and queues its delivery; returns once it is accepted, has failed, or waits for a dialog. */
  async enqueue(input: DeliverBody): Promise<void> {
    this.send({ kind: "input.received", input_id: input.input_id });
    const epoch = this.inputEpoch;
    this.delivering++;
    this.delivery = this.delivery.then(() => this.deliverQueued(input, epoch));
    await this.ui.runUntilDialog(() => this.delivery);
  }

  private async deliverQueued(input: DeliverBody, epoch: number): Promise<void> {
    const current = (): boolean => epoch === this.inputEpoch;

    try {
      if (current()) await this.deliver(input, current);
    } catch (error) {
      if (current()) this.send(workerError("delivery_failed", error instanceof Error ? error.message : "input was rejected"));
    } finally {
      this.delivering--;
    }
  }

  /** Resolves once pi accepted the input (rejects when it refused it); a later failure of the run is `session_failed`. */
  private async deliver(input: DeliverBody, active: () => boolean): Promise<void> {
    const session = this.session;
    const runFailed = (cause: unknown): void => { this.send(workerError("session_failed", cause instanceof Error ? cause.message : "the turn failed")); };

    if (session.isStreaming) {
      await (input.mode === "steer" ? session.steer(input.text) : session.followUp(input.text));

      return;
    }

    const space = input.text.indexOf(" ");
    const name = input.text.startsWith("/") ? input.text.slice(1, space < 0 ? undefined : space) : undefined;

    // An extension command has no preflight to await: it runs extension code at once, like a turn, and may wait for a
    // dialog. It is not counted as a delivery in progress, so session commands (a reload that replaces its dialogs,
    // say) stay available while it runs.
    if (name !== undefined && session.extensionRunner.getCommand(name)) {
      session.prompt(input.text, { expandPromptTemplates: true, source: "rpc" }).catch(runFailed);

      return;
    }

    await new Promise<void>((resolve, reject) => {
      let accepted = false;
      const close = this.gate.open(active);

      session.prompt(input.text, {
        expandPromptTemplates: true,
        source: "rpc",
        preflightResult: () => {
          close();
          accepted = true;
          resolve();
        },
      }).then(() => { close(); resolve(); }, (cause: unknown) => {
        close();

        if (accepted) runFailed(cause);
        else reject(cause);
      });
    });
  }

  /** Cancels pending inputs, clears pi's queues (reported by count only) and aborts the turn, compaction and retries. */
  async abort(): Promise<void> {
    this.cancelInputs();
    const session = this.session;
    const cleared = session.clearQueue();
    const steering = cleared.steering.length;
    const followUp = cleared.followUp.length;

    if (steering + followUp > 0) this.mapper.system("Queue cleared", `${steering} steering and ${followUp} follow-up message(s) were discarded before abort.`, false);
    await session.abort();
  }

  private async reload(resources: WorkerResources | undefined): Promise<void> {
    this.resources = resources;
    this.loader?.setResources(resources);
    this.ui.advance();
    await this.session.reload();
    this.announceChange("reload");
  }

  /** Runs one session command, or refuses it while an input is being delivered or another command runs. */
  async command(body: CommandBody): Promise<ResultBody> {
    const result: ResultBody = { kind: "session.result", command_id: body.command_id, ok: true, message: "" };

    if (this.delivering > 0) return { ...result, ok: false, message: "input delivery is in progress; answer its dialog, stop or wait for it to finish" };

    if (this.commandRunning) return { ...result, ok: false, message: "another session command is in progress" };
    this.commandRunning = true;

    try {
      return { ...result, ...await this.execute(body.command) };
    } catch (error) {
      return { ...result, ok: false, message: safeMessage(error instanceof Error ? error.message : "session command failed") };
    } finally {
      this.commandRunning = false;
    }
  }

  private requireIdle(what: string): void {
    if (this.session.isStreaming || this.session.isCompacting) throw new Error(`${what} needs an idle session; stop or wait for the current turn`);
  }

  private async execute(command: SessionCommand): Promise<CommandOutput> {
    const runtime = this.runtime;
    const session = runtime.session;

    switch (command.op) {
      case "tree":
        return this.tree(session);
      case "navigate":
        await this.navigate(session, command.entry_id);
        break;
      case "fork":
        this.requireIdle("forking");
        await this.replaceSession("fork", () => runtime.fork(command.entry_id, { position: command.position }));
        break;
      case "import":
        this.requireIdle("importing");

        if (!isInside(join(this.start.session_dir, "imports"), command.path)) throw new Error("imports are read only from the service's import directory");
        await this.replaceSession("import", () => runtime.importFromJsonl(command.path, this.start.cwd));
        break;
      case "export":
        if (!isInside(join(this.start.session_dir, "exports"), command.path)) throw new Error("exports are written only to the service's export directory");
        mkdirSync(dirname(command.path), { recursive: true, mode: 0o700 });
        session.exportToJsonl(command.path);
        break;
      case "compact":
        this.requireIdle("compaction");
        await session.compact(command.instructions);
        this.announceChange("compact");
        break;
      case "reload":
        this.requireIdle("reload");

        if (!isInside(this.start.session_dir, command.resources.profile_dir)) throw new Error("the profile snapshot must be inside session_dir");
        await this.reload(command.resources);
        break;
      case "configure":
        await this.configure(runtime, command.model, command.thinking);
        break;
      default: {
        const unhandled: never = command;

        return unhandled;
      }
    }

    return {};
  }

  private tree(session: AgentSession): CommandOutput {
    const manager = session.sessionManager;
    const output: CommandOutput = treePage(manager.getEntries(), id => manager.getLabel(id));
    const leaf = manager.getLeafId();

    if (leaf !== null) output.leaf_id = leaf;

    return output;
  }

  private async navigate(session: AgentSession, entryId: string): Promise<void> {
    this.requireIdle("tree navigation");

    if (!session.sessionManager.getEntry(entryId)) throw new Error("no such session entry");
    const outcome = await session.navigateTree(entryId, { summarize: false });

    if (outcome.cancelled) throw new Error("tree navigation was cancelled by an extension");
    this.announceChange("navigate");
  }

  /** Replaces the session; the rebind it triggers reports `reason` unless an extension cancels it. */
  private async replaceSession(reason: "fork" | "import", replace: () => Promise<{ cancelled: boolean }>): Promise<void> {
    this.reason = reason;
    const outcome = await replace();

    if (!outcome.cancelled) return;
    this.reason = "start";
    throw new Error(`${reason} was cancelled by an extension`);
  }

  private async configure(runtime: AgentSessionRuntime, model: Model, thinking: ThinkingLevel): Promise<void> {
    this.requireIdle("changing the model");
    const session = runtime.session;
    const registered = runtime.services.modelRuntime.getModel(model.provider, model.id);

    if (!registered) throw new Error("the requested model is not registered");
    await session.setModel(registered);
    this.model = model;
    session.setThinkingLevel(thinking);
    this.thinking = thinking;
    this.announceChange("configure");
  }

  async dispose(): Promise<void> {
    this.unsubscribe?.();
    await this.runtimeHost?.dispose();
  }
}
