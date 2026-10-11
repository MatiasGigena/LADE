// LADE-profile resources for the pi session: the service materialises a snapshot of the LADE profile
// (never the user's own pi profile) inside the session directory, approves executable extensions by
// content hash, and the worker loads exactly that set. Repository `.pi/` content, the agent dir's
// auto-discovery, `SYSTEM.md`/`APPEND_SYSTEM.md`, themes and packages stay disabled.
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync } from "node:fs";
import { basename, isAbsolute, join, relative, sep } from "node:path";
import { DefaultResourceLoader } from "@earendil-works/pi-coding-agent";
import type { LoadExtensionsResult, PromptTemplate, ResourceDiagnostic, ResourceLoader, Skill, SettingsManager, Theme } from "@earendil-works/pi-coding-agent";
import type { LoadedResource, WorkerResources } from "./wire.ts";

type ExtensionPaths = Parameters<ResourceLoader["extendResources"]>[0];

/** Directory-extension hashing bounds (mirrored by the service's `profile` module). */
export const MAX_EXTENSION_FILES = 256;

export const MAX_EXTENSION_BYTES = 8 * 1024 * 1024;

const MAX_DEPTH = 8;

/** Context file names pi looks for in one directory, in pi's order (first match wins). */
export const CONTEXT_FILES = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

export class HashError extends Error {}

function walk(root: string, dir: string, depth: number, files: string[]): void {
  if (depth > MAX_DEPTH) throw new HashError("extension directory is nested too deeply");

  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    const stat = lstatSync(path);

    if (stat.isSymbolicLink()) throw new HashError("extension contains a symbolic link");

    if (stat.isDirectory()) walk(root, path, depth + 1, files);
    else if (stat.isFile()) {
      files.push(relative(root, path).split(sep).join("/"));

      if (files.length > MAX_EXTENSION_FILES) throw new HashError("extension has too many files");
    } else throw new HashError("extension contains a special file");
  }
}

/**
 * Content hash of one extension entry. A file hashes as its bytes; a directory as the sha256 of
 * `sha256sum`-style lines (`<hex>  <relative path>\n`) of every regular file, sorted by path bytes.
 * Symbolic links and special files are refused, so a hash always covers exactly what pi can load.
 */
export function extensionHash(path: string): string {
  const stat = lstatSync(path);

  if (stat.isSymbolicLink()) throw new HashError("extension is a symbolic link");

  if (stat.isFile()) {
    if (stat.size > MAX_EXTENSION_BYTES) throw new HashError("extension is too large");

    return createHash("sha256").update(readFileSync(path)).digest("hex");
  }

  if (!stat.isDirectory()) throw new HashError("extension is not a file or directory");
  const files: string[] = [];
  walk(path, path, 0, files);
  files.sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
  const manifest = createHash("sha256");
  let total = 0;

  for (const file of files) {
    const bytes = readFileSync(join(path, file));
    total += bytes.length;

    if (total > MAX_EXTENSION_BYTES) throw new HashError("extension is too large");
    manifest.update(`${createHash("sha256").update(bytes).digest("hex")}  ${file}\n`);
  }

  return manifest.digest("hex");
}

interface ContextFile { path: string; content: string }

function contextFile(dir: string, names: readonly string[]): ContextFile | undefined {
  for (const name of names) {
    const path = join(dir, name);

    if (!existsSync(path) || !lstatSync(path).isFile()) continue;

    return { path, content: readFileSync(path, "utf8").replace(/^﻿/, "") };
  }

  return undefined;
}

/** Precedence: the LADE profile's AGENTS.md first, then the worktree root's own context file (pi order). */
export function contextFiles(profileDir: string | undefined, cwd: string): ContextFile[] {
  const found: ContextFile[] = [];
  const profile = profileDir === undefined ? undefined : contextFile(profileDir, ["AGENTS.md"]);

  if (profile) found.push(profile);
  const repository = contextFile(cwd, CONTEXT_FILES);

  if (repository) found.push(repository);

  return found;
}

interface Verified { paths: string[]; report: LoadedResource[] }

function within(parent: string, path: string): boolean {
  const rel = relative(parent, path);

  return rel.length > 0 && !rel.startsWith("..") && !isAbsolute(rel);
}

/** Re-hash every approved extension; only exact matches inside `<profile>/extensions` are loaded. */
export function verifyExtensions(resources: WorkerResources | undefined): Verified {
  const verified: Verified = { paths: [], report: [] };

  if (!resources) return verified;
  const root = join(resources.profile_dir, "extensions");

  for (const extension of resources.extensions) {
    const refuse = (message: string): void => {
      verified.report.push({ kind: "extension", name: extension.name, source: "lade_profile", status: "refused", sha256: extension.sha256, message });
    };

    if (!within(root, extension.path)) { refuse("extension path is outside the LADE profile"); continue; }

    let actual: string;

    try { actual = extensionHash(extension.path); } catch (error) {
      refuse(error instanceof HashError ? error.message : "extension could not be hashed");
      continue;
    }

    if (actual !== extension.sha256) { refuse("extension content changed since it was approved; approve the new version first"); continue; }

    verified.paths.push(extension.path);
  }

  return verified;
}

export interface LoaderInputs {
  cwd: string;
  agentDir: string;
  settingsManager: SettingsManager;
  systemNote: string;
  resources: WorkerResources | undefined;
}

/**
 * A `ResourceLoader` whose inputs can change between reloads (a new approval set). Each reload
 * builds a fresh `DefaultResourceLoader` with discovery disabled and only explicit LADE-profile
 * paths; every getter delegates to the current one.
 */
export class LadeResourceLoader implements ResourceLoader {
  private inputs: LoaderInputs;
  private inner: DefaultResourceLoader;
  private refused: LoadedResource[] = [];
  constructor(inputs: LoaderInputs) { this.inputs = inputs; this.inner = this.build(); }

  setResources(resources: WorkerResources | undefined): void {
    this.inputs = { ...this.inputs, resources };
  }

  private build(): DefaultResourceLoader {
    const { cwd, agentDir, settingsManager, systemNote, resources } = this.inputs;
    const profile = resources?.profile_dir;
    const existing = (name: string): string[] => profile !== undefined && existsSync(join(profile, name)) ? [join(profile, name)] : [];
    const verified = verifyExtensions(resources);
    this.refused = verified.report;

    return new DefaultResourceLoader({
      cwd,
      agentDir,
      settingsManager,
      noExtensions: true,
      noSkills: true,
      noPromptTemplates: true,
      noThemes: true,
      noContextFiles: true,
      additionalExtensionPaths: verified.paths,
      additionalSkillPaths: existing("skills"),
      additionalPromptTemplatePaths: existing("prompts"),
      agentsFilesOverride: () => ({ agentsFiles: contextFiles(profile, cwd) }),
      systemPromptOverride: () => undefined,
      appendSystemPromptOverride: () => systemNote.length > 0 ? [systemNote] : [],
    });
  }

  async reload(): Promise<void> {
    this.inner = this.build();
    await this.inner.reload();
  }

  getExtensions(): LoadExtensionsResult { return this.inner.getExtensions(); }

  getSkills(): { skills: Skill[]; diagnostics: ResourceDiagnostic[] } { return this.inner.getSkills(); }

  getPrompts(): { prompts: PromptTemplate[]; diagnostics: ResourceDiagnostic[] } { return this.inner.getPrompts(); }

  getThemes(): { themes: Theme[]; diagnostics: ResourceDiagnostic[] } { return this.inner.getThemes(); }

  getAgentsFiles(): { agentsFiles: Array<{ path: string; content: string }> } { return this.inner.getAgentsFiles(); }

  getSystemPrompt(): string | undefined { return this.inner.getSystemPrompt(); }

  getSystemPromptSource(): { path: string } | undefined { return this.inner.getSystemPromptSource(); }

  getAppendSystemPrompt(): string[] { return this.inner.getAppendSystemPrompt(); }

  getAppendSystemPromptSources(): Array<{ path: string }> { return this.inner.getAppendSystemPromptSources(); }

  extendResources(paths: ExtensionPaths): void { this.inner.extendResources(paths); }

  /** What this generation loaded, refused or failed to load (names and hashes only). */
  report(): LoadedResource[] {
    const resources = this.inputs.resources;
    const profile = resources?.profile_dir;
    const label = (path: string): string => basename(path).slice(0, 256) || "resource";
    const loaded: LoadedResource[] = [];

    for (const skill of this.getSkills().skills) loaded.push({ kind: "skill", name: skill.name.slice(0, 256) || "skill", source: "lade_profile", status: "loaded" });

    for (const prompt of this.getPrompts().prompts) loaded.push({ kind: "prompt", name: prompt.name.slice(0, 256) || "prompt", source: "lade_profile", status: "loaded" });

    for (const file of this.getAgentsFiles().agentsFiles) loaded.push({ kind: "context", name: label(file.path), source: profile !== undefined && within(profile, file.path) ? "lade_profile" : "repository", status: "loaded" });
    const byPath = new Map((resources?.extensions ?? []).map(extension => [extension.path, extension]));
    const extensions = this.getExtensions();

    for (const extension of extensions.extensions) {
      const approved = byPath.get(extension.path) ?? byPath.get(extension.resolvedPath);

      if (approved) loaded.push({ kind: "extension", name: approved.name, source: "lade_profile", status: "loaded", sha256: approved.sha256 });
    }

    for (const failure of extensions.errors) {
      const approved = byPath.get(failure.path);
      const entry: LoadedResource = { kind: "extension", name: approved?.name ?? label(failure.path), source: "lade_profile", status: "failed", message: failure.error.replace(/\p{Cc}+/gu, " ").slice(0, 512) };

      if (approved) entry.sha256 = approved.sha256;
      loaded.push(entry);
    }

    return [...loaded, ...this.refused].slice(0, 256);
  }
}
