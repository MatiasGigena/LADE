#!/usr/bin/env python3
"""Memory and CPU sampling for scripts/e2e-perf (test tooling, not shipped).

    scripts/perf-sample.py memory STATE_DIR PIDS_DIR PHASE_FILE OUT_CSV
    scripts/perf-sample.py cpu STATE_DIR PIDS_DIR SECONDS LABEL

PIDS_DIR holds `desktop.pid` and `laded.pid` (re-read every sample, the processes are restarted).
Process groups, following docs/planning/performance-budgets.md: the client is the window and its
descendants; a worker is every sandbox process whose command line carries STATE_DIR/sessions (bwrap
and node) and its descendants; the service group is laded and every other descendant (Git, search,
gateway helpers). `memory` writes one CSV row per second (PSS in KiB from smaps_rollup, phase
label from PHASE_FILE) until the PIDS_DIR file `stop` exists; `cpu` prints the mean CPU of each
group over SECONDS as a percent of one logical core.
"""
import os
import sys
import time

TICKS = os.sysconf('SC_CLK_TCK')


def processes():
    parent, cmdline = {}, {}
    for entry in os.listdir('/proc'):
        if not entry.isdigit():
            continue
        try:
            with open(f'/proc/{entry}/stat', encoding='utf-8', errors='replace') as f:
                stat = f.read()
            with open(f'/proc/{entry}/cmdline', 'rb') as f:
                cmdline[int(entry)] = f.read().replace(b'\0', b' ').decode(errors='replace')
        except OSError:
            continue
        parent[int(entry)] = int(stat.rsplit(')', 1)[1].split()[1])
    return parent, cmdline


def descendants(root, parent):
    children = {}
    for pid, ppid in parent.items():
        children.setdefault(ppid, []).append(pid)
    found, stack = set(), [root]
    while stack:
        pid = stack.pop()
        if pid in found or pid not in parent:
            continue
        found.add(pid)
        stack.extend(children.get(pid, []))
    return found


def read_pid(pids_dir, name):
    try:
        with open(os.path.join(pids_dir, name), encoding='utf-8') as f:
            return int(f.read().strip())
    except (OSError, ValueError):
        return None


def groups(state, pids_dir):
    parent, cmdline = processes()
    marker = os.path.join(state, 'sessions')
    workers = set()
    for pid, line in cmdline.items():
        if marker in line:
            workers |= descendants(pid, parent)
    desktop, laded = read_pid(pids_dir, 'desktop.pid'), read_pid(pids_dir, 'laded.pid')
    client = descendants(desktop, parent) if desktop else set()
    service = (descendants(laded, parent) - workers) if laded else set()
    return {'client': client, 'service': service, 'worker': workers}


def pss(pids):
    total = 0
    for pid in pids:
        try:
            with open(f'/proc/{pid}/smaps_rollup', encoding='utf-8') as f:
                for line in f:
                    if line.startswith('Pss:'):
                        total += int(line.split()[1])
                        break
        except OSError:
            pass
    return total


def cpu_ticks(pids):
    total = 0
    for pid in pids:
        try:
            with open(f'/proc/{pid}/stat', encoding='utf-8', errors='replace') as f:
                fields = f.read().rsplit(')', 1)[1].split()
            total += int(fields[11]) + int(fields[12])
        except (OSError, IndexError, ValueError):
            pass
    return total


def memory(state, pids_dir, phase_file, out):
    with open(out, 'w', encoding='utf-8') as csv:
        csv.write('t_ms,client_kib,service_kib,worker_kib,phase\n')
        while not os.path.exists(os.path.join(pids_dir, 'stop')):
            g = groups(state, pids_dir)
            try:
                with open(phase_file, encoding='utf-8') as f:
                    phase = f.read().strip()
            except OSError:
                phase = ''
            values = [str(pss(g[name])) if g[name] else '' for name in ('client', 'service', 'worker')]
            csv.write(f'{int(time.monotonic() * 1000)},{",".join(values)},{phase}\n')
            csv.flush()
            time.sleep(1)


def cpu(state, pids_dir, seconds, label):
    before = {name: cpu_ticks(pids) for name, pids in groups(state, pids_dir).items()}
    time.sleep(seconds)
    after_groups = groups(state, pids_dir)
    parts = []
    for name, pids in after_groups.items():
        if not pids:
            parts.append(f'{name} —')
            continue
        used = (cpu_ticks(pids) - before[name]) / TICKS
        parts.append(f'{name} {100 * used / seconds:.2f} %')
    print(f'{label} ({seconds} s): ' + ', '.join(parts))


def main():
    args = sys.argv[1:]
    if len(args) == 5 and args[0] == 'memory':
        memory(*args[1:])
    elif len(args) == 5 and args[0] == 'cpu':
        cpu(args[1], args[2], int(args[3]), args[4])
    else:
        sys.exit(__doc__.strip().splitlines()[2].strip())


if __name__ == '__main__':
    main()
