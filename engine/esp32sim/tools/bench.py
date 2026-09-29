#!/usr/bin/env python3
"""Interleaved A/B benchmark for emulator builds.

    tools/bench.py --rounds 5 --label base=/path/a --label new=/path/b -- <emulator args...>

Runs every binary once per round, in turn, so slow drifts in background load hit all of them
equally; reports the best and median wall time per binary and the guest instruction count
(which must agree across binaries, or they are not running the same thing)."""
import argparse, re, statistics, subprocess, sys, time

ap = argparse.ArgumentParser()
ap.add_argument('--rounds', type=int, default=5)
ap.add_argument('--label', action='append', required=True, help='name=path/to/esp32sim')
ap.add_argument('args', nargs=argparse.REMAINDER)
a = ap.parse_args()
args = a.args[1:] if a.args and a.args[0] == '--' else a.args
bins = [l.split('=', 1) for l in a.label]
res = {n: [] for n, _ in bins}
insns = {}
for r in range(a.rounds):
    for name, path in bins:
        t = time.perf_counter()
        out = subprocess.run([path] + args, capture_output=True, text=True).stderr
        dt = time.perf_counter() - t
        m = re.search(r'core0 (\d+) \+ core1 (\d+) insns', out)
        if not m: sys.exit(f'{name}: no stop line\n{out[-800:]}')
        n = int(m.group(1)) + int(m.group(2))
        insns.setdefault(name, n)
        res[name].append(dt)
        print(f'  round {r + 1} {name:>8}: {dt:6.2f} s  ({n / dt / 1e6:6.1f} Minsn/s)', file=sys.stderr)
base = None
for name, _ in bins:
    best, med = min(res[name]), statistics.median(res[name])
    rel = '' if base is None else f'  {base / best:5.3f}x vs {bins[0][0]}'
    if base is None: base = best
    print(f'{name:>8}: best {best:6.2f} s  median {med:6.2f} s  {insns[name] / best / 1e6:6.1f} Minsn/s{rel}')
counts = set(insns.values())
if len(counts) > 1: print('WARNING: instruction counts differ:', insns)
