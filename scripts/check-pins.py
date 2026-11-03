#!/usr/bin/env python3
"""Check the supported local build inputs; never install or alter host packages."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib

root = Path(__file__).resolve().parent.parent
pins = json.loads((root/'contracts/toolchain.json').read_text())
def output(args):
    return subprocess.check_output(args, cwd=root, text=True, env={**os.environ, 'CARGO_HOME':str(root/'.tools/cargo'), 'RUSTUP_HOME':str(root/'.tools/rustup')}).strip()

assert output(['node','--version']) == 'v'+pins['node'], 'Node version mismatch'
assert output(['npm','--version']) == pins['npm'], 'npm version mismatch'
assert output([str(root/'scripts/cargo'),'--version']).split()[1] == pins['rust'], 'Cargo version mismatch'
assert output([str(root/'.tools/cargo/bin/rustc'),'--version']).split()[1] == pins['rust'], 'Rust version mismatch'
assert tomllib.loads((root/'rust-toolchain.toml').read_text())['toolchain']['channel'] == pins['rust']
assert (root/'.node-version').read_text().strip() == pins['node']
if '--portable' not in sys.argv:
    for name, version in pins['pkg_config'].items():
        assert output(['pkg-config','--modversion',name]) == version, f'native ABI baseline mismatch: {name}'
    for name, version in pins['native_packages'].items():
        assert output(['dpkg-query','-W','-f=${Version}',name]) == version, f'native package baseline mismatch: {name}'
vendor=root/'vendor/pi-coding-agent'
manifest=json.loads((vendor/'manifest.json').read_text())
for path, expected in manifest['files'].items():
    assert hashlib.sha256((vendor/path).read_bytes()).hexdigest() == expected, f'pi input changed: {path}'
assert json.loads((vendor/'upstream/package.json').read_text())['version'] == manifest['version'] == '1.5.0'
lock=json.loads((vendor/'upstream/install-lock/package-lock.json').read_text())
assert lock['packages']['']['version'] == manifest['version']
for name, pkg in lock['packages'].items():
    if name.rsplit('node_modules/', 1)[-1].startswith('@earendil-works/'):
        assert pkg['version'] == manifest['version'], f'pi package mismatch: {name}'
worker=json.loads((root/'packages/pi-worker/package.json').read_text())
assert worker['engines'] == {'node':pins['node'],'npm':pins['npm']}
assert worker['version'] == pins['bundle']
assert tomllib.loads((root/'Cargo.toml').read_text())['workspace']['package']['version'] == pins['bundle']
schema=json.loads((root/'contracts/v1/schema.json').read_text())
assert schema['properties']['protocol']['const'] == pins['protocol']
assert schema['properties']['bundle']['const'] == pins['bundle']
print(json.dumps({'native_checked':'--portable' not in sys.argv, 'pins':pins},indent=2))
