#!/usr/bin/env python3
import json
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent.parent
rust=json.loads(subprocess.check_output([str(root/'scripts/cargo'),'run','--quiet','--locked','-p','lade-protocol','--example','fixtures'],cwd=root))
ts=json.loads(subprocess.check_output(['node','--experimental-strip-types','packages/pi-worker/test/report.ts'],cwd=root))
assert rust == ts, 'Rust/TypeScript fixture results differ'
print(json.dumps({'shared_cases':len(rust),'outcomes':rust},indent=2))
