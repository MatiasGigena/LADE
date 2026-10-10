import { mkdirSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import {
  createAgentSession,
  DefaultResourceLoader,
  ModelRuntime,
  SessionManager,
  SettingsManager,
} from "@earendil-works/pi-coding-agent";
import type { AgentSession } from "@earendil-works/pi-coding-agent";
import { FIXTURE_PROVIDER, fixtureProvider } from "./fixture.ts";
import type { DeliverBody, StartBody } from "./wire.ts";
import type { WorkerErrorCode } from "../dto.ts";

/** Built-in coding tools; they run inside the outer bwrap sandbox, not a pi-level one. */
export const WORKER_TOOLS = ["read", "bash", "edit", "write"];

export class StartError extends Error {
  readonly code: WorkerErrorCode;
  constructor(code: WorkerErrorCode, message: string) { super(message); this.code = code; }
}

export interface ProfilePaths { agentDir: string; sessionsDir: string }

/** Profile layout inside the Objective's session_dir; the real ~/.pi is never consulted. */
export function profilePaths(sessionDir: string): ProfilePaths {
  return { agentDir: join(sessionDir, "agent"), sessionsDir: join(sessionDir, "sessions") };
}

/**
 * Create or resume the Objective's pi session. Resource discovery (extensions, skills, prompt
 * templates, themes, context files, SYSTEM.md/APPEND_SYSTEM.md) is disabled and settings are
 * in-memory, so neither the user's profile nor repository-controlled `.pi/` content is loaded.
 */
export async function openSession(start: StartBody): Promise<AgentSession> {
  if (!isAbsolute(start.session_dir) || !isAbsolute(start.cwd)) throw new StartError("startup_failed", "session_dir and cwd must be absolute paths");
  const { agentDir, sessionsDir } = profilePaths(start.session_dir);
  mkdirSync(agentDir, { recursive: true, mode: 0o700 });
  mkdirSync(sessionsDir, { recursive: true, mode: 0o700 });
  // getAgentDir() fallbacks inside the SDK resolve here too; set before a session object exists.
  process.env.PI_CODING_AGENT_DIR = agentDir;
  process.env.PI_CODING_AGENT_SESSION_DIR = sessionsDir;

  const modelRuntime = await ModelRuntime.create({
    authPath: join(agentDir, "auth.json"),
    modelsPath: null,
    modelsStorePath: join(agentDir, "models-store.json"),
    allowModelNetwork: false,
  });

  modelRuntime.registerProvider(FIXTURE_PROVIDER, fixtureProvider());
  const model = modelRuntime.getModel(start.model.provider, start.model.id);

  if (!model) throw new StartError("startup_failed", "the requested model is not registered");

  const settingsManager = SettingsManager.inMemory({
    defaultProvider: model.provider,
    defaultModel: model.id,
    quietStartup: true,
    enableInstallTelemetry: false,
    enableAnalytics: false,
    defaultProjectTrust: "never",
    steeringMode: "one-at-a-time",
    followUpMode: "one-at-a-time",
  });

  const resourceLoader = new DefaultResourceLoader({
    cwd: start.cwd,
    agentDir,
    settingsManager,
    noExtensions: true,
    noSkills: true,
    noPromptTemplates: true,
    noThemes: true,
    noContextFiles: true,
    systemPromptOverride: () => undefined,
    appendSystemPromptOverride: () => start.system_note.length > 0 ? [start.system_note] : [],
  });

  await resourceLoader.reload();
  const sessionManager = start.resume ? SessionManager.continueRecent(start.cwd, sessionsDir) : SessionManager.create(start.cwd, sessionsDir);

  const { session } = await createAgentSession({
    cwd: start.cwd,
    agentDir,
    modelRuntime,
    model,
    tools: WORKER_TOOLS,
    resourceLoader,
    sessionManager,
    settingsManager,
  });

  return session;
}

/**
 * Delivery-mode mapping. While a run is active: `steer` → steer(), `follow_up` → followUp(), and
 * `prompt` → followUp() (pi rejects an unqueued prompt during streaming). While idle every mode
 * starts a run with prompt(). Resolves once pi has accepted or queued the text, not when the run
 * ends; `onRunError` receives failures of the started run.
 */
export function deliver(session: AgentSession, input: DeliverBody, onRunError: (error: Error) => void): Promise<void> {
  if (session.isStreaming) return input.mode === "steer" ? session.steer(input.text) : session.followUp(input.text);

  return new Promise((resolve, reject) => {
    let accepted = false;

    session.prompt(input.text, {
      expandPromptTemplates: false,
      source: "rpc",
      preflightResult: ok => { accepted = ok;

 if (ok) resolve(); },
    }).then(() => resolve(), (error: Error) => {
      if (accepted) onRunError(error);
      else reject(error);
    });
  });
}

/**
 * Pinned pi ordering for an interrupt: drop queued steering/follow-up first (reported through
 * `onCleared` before the run settles), then abort and wait for the session to become idle.
 */
export async function abort(session: AgentSession, onCleared: (steering: number, followUp: number) => void): Promise<void> {
  const cleared = session.clearQueue();

  if (cleared.steering.length + cleared.followUp.length > 0) onCleared(cleared.steering.length, cleared.followUp.length);
  await session.abort();
}
