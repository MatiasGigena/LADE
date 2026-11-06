import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync } from "node:fs";
import { basename, join, relative, sep } from "node:path";
import { DefaultResourceLoader } from "@earendil-works/pi-coding-agent";
import type { ExtensionFactory, LoadExtensionsResult, PromptTemplate, ResourceDiagnostic, ResourceLoader, Skill, SettingsManager, Theme } from "@earendil-works/pi-coding-agent";
import type { LoadedResource, WorkerResources } from "../dto.ts";
import { isInside } from "./paths.ts";
import { prefix, safeMessage } from "./text.ts";

type ExtensionPaths = Parameters<ResourceLoader["extendResources"]>[0];

const MAX_EXTENSION_FILES = 256;

const MAX_EXTENSION_BYTES = 8 * 1024 * 1024;

const MAX_DEPTH = 8;

const MAX_RESOURCE_NAME = 256;

const MAX_RESOURCES = 256;

const CONTEXT_FILES = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

class HashError extends Error {
  readonly name = "HashError";
}

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

function contextFiles(profileDir: string | undefined, cwd: string): ContextFile[] {
  const found: ContextFile[] = [];
  const profile = profileDir === undefined ? undefined : contextFile(profileDir, ["AGENTS.md"]);

  if (profile) found.push(profile);
  const repository = contextFile(cwd, CONTEXT_FILES);

  if (repository) found.push(repository);

  return found;
}

interface Verified { paths: string[]; report: LoadedResource[] }

function verifyExtensions(resources: WorkerResources | undefined): Verified {
  const verified: Verified = { paths: [], report: [] };

  if (!resources) return verified;
  const root = join(resources.profile_dir, "extensions");

  for (const extension of resources.extensions) {
    const refuse = (message: string): void => {
      verified.report.push({ kind: "extension", name: extension.name, source: "lade_profile", status: "refused", sha256: extension.sha256, message });
    };

    if (!isInside(root, extension.path)) { refuse("extension path is outside the LADE profile"); continue; }

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

interface LoaderInputs {
  cwd: string;
  agentDir: string;
  settingsManager: SettingsManager;
  systemNote: string;
  resources: WorkerResources | undefined;
  inputGate: ExtensionFactory;
}

export class LadeResourceLoader implements ResourceLoader {
  private inputs: LoaderInputs;
  private inner: DefaultResourceLoader;
  private refused: LoadedResource[] = [];
  constructor(inputs: LoaderInputs) { this.inputs = inputs; this.inner = this.build(); }

  setResources(resources: WorkerResources | undefined): void {
    this.inputs = { ...this.inputs, resources };
  }

  private build(): DefaultResourceLoader {
    const { cwd, agentDir, settingsManager, systemNote, resources, inputGate } = this.inputs;
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
      extensionFactories: [inputGate],
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

  report(): LoadedResource[] {
    const resources = this.inputs.resources;
    const profile = resources?.profile_dir;
    const label = (path: string): string => prefix(basename(path), MAX_RESOURCE_NAME) || "resource";
    const loaded: LoadedResource[] = [];

    for (const skill of this.getSkills().skills) loaded.push({ kind: "skill", name: prefix(skill.name, MAX_RESOURCE_NAME) || "skill", source: "lade_profile", status: "loaded" });

    for (const prompt of this.getPrompts().prompts) loaded.push({ kind: "prompt", name: prefix(prompt.name, MAX_RESOURCE_NAME) || "prompt", source: "lade_profile", status: "loaded" });

    for (const file of this.getAgentsFiles().agentsFiles) loaded.push({ kind: "context", name: label(file.path), source: profile !== undefined && isInside(profile, file.path) ? "lade_profile" : "repository", status: "loaded" });
    const byPath = new Map((resources?.extensions ?? []).map(extension => [extension.path, extension]));
    const extensions = this.getExtensions();

    for (const extension of extensions.extensions) {
      const approved = byPath.get(extension.path) ?? byPath.get(extension.resolvedPath);

      if (approved) loaded.push({ kind: "extension", name: approved.name, source: "lade_profile", status: "loaded", sha256: approved.sha256 });
    }

    for (const failure of extensions.errors) {
      const approved = byPath.get(failure.path);
      const entry: LoadedResource = { kind: "extension", name: approved?.name ?? label(failure.path), source: "lade_profile", status: "failed", message: safeMessage(failure.error) };

      if (approved) entry.sha256 = approved.sha256;
      loaded.push(entry);
    }

    return [...loaded, ...this.refused].slice(0, MAX_RESOURCES);
  }
}
