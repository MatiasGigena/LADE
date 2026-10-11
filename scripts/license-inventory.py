#!/usr/bin/env python3
"""Shipped-dependency license inventory (IMP-014).

    python3 scripts/license-inventory.py --check          # CI: every shipped dependency has license info
    python3 scripts/license-inventory.py --bundle DIR     # write the inventory into a bundle

Rust: the crates linked into `laded` and `lade-desktop` for x86_64 Linux, from `cargo metadata
--offline --locked` (normal dependencies only; build scripts, proc-macro crates and dev
dependencies are build inputs, not shipped code). Workspace crates are LADE itself.
Node: in --check mode the production entries of packages/pi-worker/package-lock.json; in --bundle
mode the packages actually present in the bundle's lib/lade/pi-worker/node_modules (their own
license files ship inside the bundle and are referenced, not copied), plus the bundled Node
runtime when present.

--bundle writes share/doc/lade/THIRD_PARTY.json, share/doc/lade/THIRD_PARTY.md and copies every
Rust crate's license files to share/doc/lade/LICENSES/rust/<crate>-<version>/. A dependency
without a license expression and without a license file fails the run. No network is used.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGET = "x86_64-unknown-linux-gnu"
SHIPPED_BINARIES = ("lade-system", "lade-desktop")
LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE|COPYRIGHT)([-._].*)?$", re.IGNORECASE)


def fail(message: str) -> None:
    print(f"license-inventory: FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def license_files(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    return sorted(p for p in directory.iterdir() if p.is_file() and LICENSE_FILE.match(p.name))


def rust_crates() -> list[dict]:
    out = subprocess.run(
        [str(ROOT / "scripts/cargo"), "metadata", "--locked", "--offline", "--format-version", "1",
         "--filter-platform", TARGET],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    meta = json.loads(out)
    packages = {p["id"]: p for p in meta["packages"]}
    workspace = set(meta["workspace_members"])
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}

    def is_proc_macro(pkg: dict) -> bool:
        return any("proc-macro" in t["kind"] for t in pkg["targets"])

    roots = [pid for pid in workspace if packages[pid]["name"] in SHIPPED_BINARIES]
    if len(roots) != len(SHIPPED_BINARIES):
        fail("workspace binaries not found in cargo metadata")
    seen: set[str] = set()
    stack = list(roots)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if not any(k["kind"] is None for k in dep["dep_kinds"]):
                continue
            if is_proc_macro(packages[dep["pkg"]]):
                continue
            stack.append(dep["pkg"])
    crates = []
    for pid in sorted(seen - workspace, key=lambda i: (packages[i]["name"], packages[i]["version"])):
        pkg = packages[pid]
        directory = Path(pkg["manifest_path"]).parent
        files = license_files(directory)
        if pkg.get("license_file"):
            extra = directory / pkg["license_file"]
            if extra.is_file() and extra not in files:
                files.append(extra)
        crates.append({
            "ecosystem": "rust",
            "name": pkg["name"],
            "version": pkg["version"],
            "license": pkg.get("license"),
            "source": pkg.get("source"),
            "repository": pkg.get("repository"),
            "_files": files,
        })
    return crates


def node_license_field(manifest: dict) -> str | None:
    value = manifest.get("license")
    if isinstance(value, dict):
        value = value.get("type")
    if isinstance(value, str) and value.strip():
        return value.strip()
    legacy = manifest.get("licenses")
    if isinstance(legacy, list):
        types = [item.get("type") for item in legacy if isinstance(item, dict) and item.get("type")]
        if types:
            return " OR ".join(types)
    return None


def node_from_lock() -> list[dict]:
    lock = json.loads((ROOT / "packages/pi-worker/package-lock.json").read_text())
    packages = []
    for path, entry in sorted(lock["packages"].items()):
        if not path or entry.get("dev") or entry.get("link"):
            continue
        packages.append({
            "ecosystem": "npm",
            "name": path.rsplit("node_modules/", 1)[-1],
            "version": entry.get("version"),
            "license": node_license_field(entry),
            "path": path,
            "_files": [],
        })
    return packages


def node_from_tree(bundle: Path) -> list[dict]:
    worker = bundle / "lib/lade/pi-worker"
    packages = []
    for manifest_path in sorted((worker / "node_modules").rglob("package.json")):
        directory = manifest_path.parent
        rel = directory.relative_to(worker)
        # A package directory is node_modules/<name> or node_modules/@scope/<name>, possibly nested.
        scoped = directory.parent.name.startswith("@") and directory.parent.parent.name == "node_modules"
        if directory.parent.name != "node_modules" and not scoped:
            continue
        try:
            manifest = json.loads(manifest_path.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(manifest, dict) or "name" not in manifest or "version" not in manifest:
            continue
        files = license_files(manifest_path.parent)
        packages.append({
            "ecosystem": "npm",
            "name": manifest["name"],
            "version": manifest["version"],
            "license": node_license_field(manifest),
            "path": str(Path("lib/lade/pi-worker") / rel),
            "license_files": [str(Path("lib/lade/pi-worker") / rel / f.name) for f in files],
            "_files": files,
        })
    return packages


def node_runtime(bundle: Path) -> list[dict]:
    runtime = bundle / "lib/lade/node"
    if not runtime.is_dir():
        return []
    version = (runtime / "VERSION").read_text().strip() if (runtime / "VERSION").is_file() else None
    files = license_files(runtime)
    return [{
        "ecosystem": "runtime",
        "name": "node",
        "version": version,
        "license": "MIT AND others (see LICENSE)",
        "path": "lib/lade/node",
        "license_files": [f"lib/lade/node/{f.name}" for f in files],
        "_files": files,
    }]


def check_entries(entries: list[dict]) -> None:
    missing = [f"{e['ecosystem']}:{e['name']}@{e['version']}" for e in entries
               if not e.get("license") and not e["_files"]]
    if missing:
        fail("no license information for: " + ", ".join(missing))


def write_bundle(bundle: Path, entries: list[dict]) -> None:
    doc = bundle / "share/doc/lade"
    licenses = doc / "LICENSES/rust"
    if licenses.exists():
        shutil.rmtree(licenses)
    public = []
    for entry in entries:
        if entry["ecosystem"] == "rust":
            target = licenses / f"{entry['name']}-{entry['version']}"
            copied = []
            for f in entry["_files"]:
                target.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(f, target / f.name)
                os.chmod(target / f.name, 0o644)
                copied.append(str((target / f.name).relative_to(bundle)))
            entry["license_files"] = copied
        public.append({k: v for k, v in entry.items() if not k.startswith("_")})
    (doc / "THIRD_PARTY.json").write_text(json.dumps({
        "format": 1,
        "note": "Shipped third-party components of this bundle. Rust: crates linked into laded and "
                "lade-desktop (proc-macro and build-only crates excluded). npm: packages in "
                "lib/lade/pi-worker/node_modules. runtime: the bundled Node, when present.",
        "components": public,
    }, indent=1, sort_keys=True) + "\n")
    lines = ["# Third-party components shipped in this bundle", "",
             "Generated by `scripts/license-inventory.py --bundle`. Rust crate license texts are in",
             "`LICENSES/rust/`; npm packages carry their own license files inside",
             "`lib/lade/pi-worker/node_modules`; machine-readable form: `THIRD_PARTY.json`.", "",
             "`libsqlite3-sys` compiles SQLite into `laded` (SQLite is in the public domain). GTK 4,",
             "GtkSourceView 5, GLib and the C library are linked dynamically from the host and are not",
             "shipped. bubblewrap, prlimit and Git are host prerequisites.", ""]
    for ecosystem, title in (("rust", "Rust crates"), ("npm", "npm packages"), ("runtime", "Runtime")):
        group = [e for e in public if e["ecosystem"] == ecosystem]
        if not group:
            continue
        lines += [f"## {title} ({len(group)})", "", "| Component | Version | License |", "| --- | --- | --- |"]
        lines += [f"| {e['name']} | {e['version']} | {e.get('license') or 'see license files'} |" for e in group]
        lines.append("")
    (doc / "THIRD_PARTY.md").write_text("\n".join(lines))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--bundle", type=Path)
    args = parser.parse_args()
    rust = rust_crates()
    if args.check:
        node = node_from_lock()
        check_entries(rust + node)
        print(f"license-inventory: ok: {len(rust)} Rust crates, {len(node)} npm packages with license information")
        return
    bundle = args.bundle.resolve()
    node = node_from_tree(bundle) + node_runtime(bundle)
    if not any(e["ecosystem"] == "npm" for e in node):
        fail(f"no npm packages found in {bundle}/lib/lade/pi-worker/node_modules")
    entries = rust + node
    check_entries(entries)
    write_bundle(bundle, entries)
    print(f"license-inventory: {len(rust)} Rust crates, {len(node)} npm packages/runtime in {bundle}/share/doc/lade")


if __name__ == "__main__":
    main()
