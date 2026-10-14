#!/usr/bin/env python3
"""Fail closed on unreviewed dependency edges or platform imports in the domain."""
import json
from pathlib import Path
import re
import subprocess
import sys

root = Path(__file__).resolve().parent.parent
metadata = json.loads(subprocess.check_output([str(root/'scripts/cargo'), 'metadata', '--locked', '--format-version', '1'], cwd=root))
packages = {p['id']: p for p in metadata['packages']}
nodes = {n['id']: n['dependencies'] for n in metadata['resolve']['nodes']}
allowed = {
    'lade-core': set(),
    'lade-protocol': {'serde', 'serde_json', 'sha2'},
    'lade-system': {'lade-core', 'lade-protocol', 'sha2', 'serde_json', 'rusqlite', 'rustix'},
    'lade-desktop': {'lade-protocol', 'gtk4', 'sourceview5'},
}
errors = []
graph = {}
for member in metadata['workspace_members']:
    p = packages[member]
    deps = {d['name'] for d in p['dependencies']}
    graph[p['name']] = sorted(deps)
    if p['name'] not in allowed or deps != allowed[p['name']]:
        errors.append(f"unexpected dependencies: {p['name']}: {sorted(deps)}")
    reachable = set()
    pending = list(nodes[member])
    while pending:
        dep = pending.pop()
        if dep in reachable: continue
        reachable.add(dep)
        pending.extend(nodes[dep])
    names = {packages[d]['name'] for d in reachable}
    if p['name'] == 'lade-desktop' and any(n.startswith(('lade-system', 'lade-core', 'rusqlite', 'sqlx', 'sqlite', 'libsqlite')) for n in names):
        errors.append('client reaches service/domain/storage')
    if p['name'] == 'lade-core' and names:
        errors.append('domain requires external crates')

for path in (root/'crates/lade-core/src').rglob('*.rs'):
    text = path.read_text()
    if re.search(r'\b(?:std|gtk4|rusqlite|sqlx|lade_protocol|lade_system)\s*::|\b(?:extern\s+crate|include!|include_str!|include_bytes!)', text):
        errors.append(f'platform/import boundary: {path.relative_to(root)}')
worker_src = root/'packages/pi-worker/src'
worker_packages = {'@earendil-works/pi-coding-agent', 'undici'}
nested_pi_ai = root/'packages/pi-worker/node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/index.js'
worker_builtins = {'node:console', 'node:crypto', 'node:events', 'node:fs', 'node:http', 'node:https', 'node:net', 'node:path', 'node:stream', 'node:tls'}
for path in worker_src.rglob('*.ts'):
    text = path.read_text()
    if re.search(r'\bany\b|import\s*\(', text):
        errors.append(f'unreviewed TS escape: {path.relative_to(root)}')
    imports = re.findall(r'(?:from|import)\s+["\']([^"\']+)["\']', text)
    is_worker = path == worker_src/'worker.ts' or worker_src/'worker' in path.parents
    def reviewed(x):
        if x.startswith(('./', '../')):
            target = (path.parent/x).resolve()
            if path == worker_src/'worker/pi-ai.ts' and target == nested_pi_ai:
                return True
            return target in (root/'contracts/v1/schema.json', root/'contracts/v1/worker-schema.json') or worker_src in target.parents
        return x == 'node:crypto' or (is_worker and (x in worker_packages or x in worker_builtins))
    if not all(reviewed(x) for x in imports):
        errors.append(f'unreviewed worker dependency: {path.relative_to(root)}')
    if re.search(r'\b(?:require|eval|Function)\s*\(', text):
        errors.append(f'unreviewed runtime loader: {path.relative_to(root)}')
if errors:
    raise SystemExit('\n'.join(errors))
print(json.dumps(graph, sort_keys=True, indent=2))
