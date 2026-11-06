#!/usr/bin/env python3
"""Exercise the pinned CLI and actual config without modifying the working tree."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
worker = root / 'packages/pi-worker'
guard = 'export function isText(value: string | number): value is string { return typeof value === "string"; }\n'
cases = [
    ('boundary-guard', 'src/schema.ts', guard, None),
    ('boundary-object-guard', 'src/schema.ts',
     'interface Entry { name: string }\n\nexport function isEntry(value: Entry | null): value is Entry { return typeof value === "object" && value !== null && !Array.isArray(value); }\n', None),
    ('boundary-non-guard', 'src/schema.ts',
     'export function isText(value: string | number): boolean { return typeof value === "string"; }\n', 'no-runtime-typeof'),
    ('nested-non-guard', 'src/schema.ts',
     'export function isText(value: string | number): value is string { return (() => typeof value === "string")(); }\n', 'no-runtime-typeof'),
    ('internal-guard', 'src/internal.ts', guard, 'no-runtime-typeof'),
    ('internal-non-guard', 'src/internal.ts',
     'export function isText(value: string | number): boolean { return typeof value === "string"; }\n', 'no-runtime-typeof'),
    ('unknown-guard-subject', 'src/schema.ts',
     'export function isText(value: unknown): value is string { return typeof value === "string"; }\n', None),
    ('unknown-parameter', 'src/schema.ts',
     'export function accept(value: unknown): void { console.log(value); }\n', 'no-unknown-parameters'),
    ('unknown-return', 'src/schema.ts',
     'export function result(): unknown { throw new Error("unavailable"); }\n', 'no-unknown-returns'),
    ('unknown-alias', 'src/schema.ts',
     'export type Input = unknown;\n', 'no-unknown-type-aliases'),
    ('reflect-get', 'src/schema.ts',
     'export function read(value: { name: string }): string { return Reflect.get(value, "name"); }\n', 'no-reflect-get'),
]
with tempfile.TemporaryDirectory(prefix='lade-lint-') as directory:
    scratch = Path(directory)
    shutil.copyfile(worker / '.oxlintrc.json', scratch / '.oxlintrc.json')
    (scratch / 'tools').symlink_to(worker / 'tools', target_is_directory=True)
    (scratch / 'node_modules').symlink_to(worker / 'node_modules', target_is_directory=True)
    (scratch / 'src').mkdir()
    for name, path, code, rule in cases:
        (scratch / path).write_text(code)
        command = [str(worker / 'node_modules/.bin/oxlint'), '-c', '.oxlintrc.json', '--format', 'json', path]
        result = subprocess.run(command, cwd=scratch, text=True, capture_output=True, check=False)
        report = json.loads(result.stdout)
        diagnostics = report['diagnostics']
        expected = [] if rule is None else ['anti-slop(' + rule + ')']
        actual = [item['code'] for item in diagnostics]
        assert result.returncode == (0 if rule is None else 1), (name, result.stdout, result.stderr)
        assert actual == expected, (name, result.stdout, result.stderr)
        assert all(item['severity'] == 'error' for item in diagnostics), (name, diagnostics)
        print(f'{name}: exit={result.returncode}, diagnostics={actual}')
print(f'{len(cases)} lint configuration cases passed (no fixes or installs)')
