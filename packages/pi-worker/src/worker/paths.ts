import { isAbsolute, join, relative, sep } from "node:path";

interface ProfilePaths { agentDir: string; sessionsDir: string }

export function profilePaths(sessionDir: string): ProfilePaths {
  return { agentDir: join(sessionDir, "agent"), sessionsDir: join(sessionDir, "sessions") };
}

/** pi's own variables, which only the launch environment sets and tool subprocesses inherit. */
export function piEnvironment(sessionDir: string): ReadonlyArray<readonly [string, string]> {
  const { agentDir, sessionsDir } = profilePaths(sessionDir);

  return [["PI_CODING_AGENT_DIR", agentDir], ["PI_CODING_AGENT_SESSION_DIR", sessionsDir], ["PI_OFFLINE", "1"], ["PI_SKIP_VERSION_CHECK", "1"], ["PI_TELEMETRY", "0"]];
}

/**
 * True when `path` is strictly below `parent`. The check is lexical and by path segment: `parent/..name` is inside,
 * `parent/../name` and `parent` itself are not. It does not resolve symbolic links.
 */
export function isInside(parent: string, path: string): boolean {
  const rel = relative(parent, path);

  return rel.length > 0 && rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel);
}
