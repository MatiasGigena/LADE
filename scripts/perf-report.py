#!/usr/bin/env python3
"""Summarises a `scripts/e2e-perf` run: presentation latencies from the client's opt-in trace
(LADE_DESKTOP_TRACE, crates/lade-desktop/src/views/trace.rs) and the memory/CPU samples.

    scripts/perf-report.py TRACE MEMORY_CSV CPU_TXT > report.txt

Latencies are measured inside the client: from the moment a request leaves the views (or a draft
edit is observed) to the store change that shows its result, and to the next frame GTK painted
after it ("painted" — rendered by the toolkit, not confirmed on screen by a compositor). Values
are milliseconds; percentiles are nearest-rank. Budgets are the accepted provisional targets of
docs/planning/performance-budgets.md; a comparison is printed, never a pass/fail verdict for the
supported host.
"""
import math
import sys
from collections import defaultdict


def nearest_rank(values, p):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(p / 100 * len(ordered)) - 1)]


def stats(values):
    if not values:
        return 'no samples'
    return (f'n={len(values)} p50={nearest_rank(values, 50)} p95={nearest_rank(values, 95)} '
            f'max={max(values)}')


def load(path):
    events = []
    for line in open(path, encoding='utf-8'):
        parts = line.rstrip('\n').split(' ', 2)
        if len(parts) < 2 or not parts[0].isdigit():
            continue
        events.append((int(parts[0]), parts[1], parts[2] if len(parts) > 2 else ''))
    return events


def next_index(events, start, predicate):
    for i in range(start, len(events)):
        if predicate(events[i]):
            return i
    return None


def paint_after(events, i):
    j = next_index(events, i + 1, lambda e: e[1] == 'paint')
    return None if j is None else events[j][0]


def measure(events, request, result, key=lambda e: ''):
    """For each request event, the first later result event: (to store, to painted) by key."""
    out = defaultdict(lambda: ([], []))
    for i, event in enumerate(events):
        if not request(event):
            continue
        j = next_index(events, i + 1, result)
        if j is None:
            continue
        painted = paint_after(events, j)
        store, paint = out[key(events[j])]
        store.append(events[j][0] - event[0])
        if painted is not None:
            paint.append(painted - event[0])
    return out


def main():
    trace, memory_csv, cpu_txt = sys.argv[1:4]
    events = load(trace)
    print('# One-worker reference trace (scripts/e2e-perf)\n')
    print('All latencies in ms: "store" = result applied to the client store; "painted" = next '
          'frame GTK painted after it.\n')

    def report(title, budget, results):
        print(f'## {title}\n')
        print(f'Budget: {budget}\n')
        for key in sorted(results):
            store, paint = results[key]
            label = f' [{key}]' if key else ''
            print(f'- to store{label}: {stats(store)}')
            print(f'- to painted{label}: {stats(paint)}')
        if not results:
            print('- no samples')
        print()

    launches = []
    for i, event in enumerate(events):
        if event[1] == 'launch':
            j = next_index(events, i + 1, lambda e: e[1] == 'objectives')
            painted = None if j is None else paint_after(events, j)
            if painted is not None:
                launches.append(painted - event[0])
    print('## Launch (process start → first painted frame with the Objectives list)\n')
    print('Budget: p95 ≤ 2 s to usable presented workbench (service already running here, '
          'OS-cache warm)\n')
    print(f'- {stats(launches)}\n')

    report('Input → first agent activity (Send → first assistant item)',
           'none for agent output (includes the worker and the fixture model); local input '
           'p95 ≤ 100 ms applies to presentation only',
           measure(events, lambda e: e[1:] == ('intent', 'Send message'),
                   lambda e: e[1] == 'activity' and e[2].endswith(' Assistant')))

    def file_key(e):
        return e[2].split(' of ')[1] if ' of ' in e[2] else e[2]

    report('Open file (Full file: request → first page shown)',
           'p95 ≤ 500 ms to first usable, version-labelled viewport (reference 1,280,000 bytes)',
           measure(events, lambda e: e[1:] == ('intent', 'Open file'),
                   lambda e: e[1] == 'viewer' and e[2].startswith('file '), file_key))
    report('Open diff (request → diff shown)',
           'p95 ≤ 500 ms (reference 4,000-line / 184,000-byte diff)',
           measure(events, lambda e: e[1:] == ('intent', 'Load diff'),
                   lambda e: e[1] == 'viewer' and e[2].startswith('diff '),
                   lambda e: e[2]))

    edit_to_saved, request_to_ack = [], []
    for i, event in enumerate(events):
        if event[1:] != ('intent', 'Save draft'):
            continue
        j = next_index(events, i + 1, lambda e: e[1] == 'draft' and ' Saved r' in e[2])
        if j is None:
            continue
        request_to_ack.append(events[j][0] - event[0])
        edits = [e[0] for e in events[:i] if e[1] == 'draft-edit']
        painted = paint_after(events, j)
        if edits and painted is not None:
            edit_to_saved.append(painted - edits[-1])
    print('## Draft save\n')
    print('Budget: request after 300 ms quiet (≥ every 2 s while typing); durable acknowledgement '
          '≤ 1 s after the request; unacknowledged edit age ≤ 3 s\n')
    print(f'- request → acknowledgement applied: {stats(request_to_ack)}')
    print(f'- last keystroke → "Saved rN" painted (includes the 300 ms quiet period): '
          f'{stats(edit_to_saved)}\n')

    report('Freeze for review (request → frozen candidate shown)', 'none set; reported',
           measure(events, lambda e: e[1:] == ('intent', 'Freeze for review'),
                   lambda e: e[1] == 'detail' and e[2].startswith('Frozen')))
    report('Integrate (request → integrated shown)', 'none set; reported',
           measure(events, lambda e: e[1:] == ('intent', 'Integrate'),
                   lambda e: e[1] == 'detail' and e[2].startswith('Integrated')))

    phases = defaultdict(lambda: defaultdict(list))
    for line in open(memory_csv, encoding='utf-8'):
        if line.startswith('t_ms'):
            continue
        t, client, service, worker, phase = line.strip().split(',')
        for name, value in (('client', client), ('service', service), ('worker', worker)):
            if value:
                phases[phase][name].append(int(value))
    print('## Memory (PSS, MiB; sampled every second)\n')
    print('Budgets: client ≤ 150 idle / ≤ 250 nominal peak; service group ≤ 64 / ≤ 128; '
          'per worker ≤ 128 / ≤ 256\n')
    print('| Phase | Client median / max | Service group median / max | Worker median / max |')
    print('| --- | --- | --- | --- |')
    for phase, values in phases.items():
        cells = []
        for name in ('client', 'service', 'worker'):
            samples = values.get(name, [])
            cells.append('—' if not samples else
                         f'{nearest_rank(samples, 50) / 1024:.1f} / {max(samples) / 1024:.1f}')
        print(f'| {phase} | ' + ' | '.join(cells) + ' |')
    print()
    print('## CPU (percent of one logical core, idle windows)\n')
    print('Budgets: client ≤ 1 %, service ≤ 0.5 %, idle worker ≤ 0.5 % (60 s mean)\n')
    for line in open(cpu_txt, encoding='utf-8'):
        print(f'- {line.strip()}')


if __name__ == '__main__':
    main()
