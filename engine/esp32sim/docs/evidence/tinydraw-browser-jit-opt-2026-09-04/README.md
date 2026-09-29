# TinyDraw browser JIT: less recompilation and execution overhead

Capture retention: summaries and compact results remain here. Full profiles, event
streams, console logs and screenshots are available through the [capture archive](../ARCHIVE.md).
For raw-file reproduction commands below, restore those files to their original paths first.

The optimized JIT completed the same fresh TinyDraw battery in **130.0002 seconds**,
down from **154.1016 seconds** for the previous JIT: 15.6% less wall time in these
runs. The interpreter on the same updated binary took 156.9248 seconds.

| Measurement | Previous JIT | Optimized JIT |
| --- | ---: | ---: |
| Battery wall time | 154.1016 s | 130.0002 s |
| Compiled blocks | 77,119 | 7,516 |
| Host compilation time | 5.4028 s | 0.6077 s |
| Compilation failures | 0 | 0 |

Compilations fell by 90.3%. The optimized run executed 4,877,588,870 instructions
through compiled blocks, including memory helpers. No instruction coverage was
added. The run reached **0.367× real time**, and real time requires about **2.72× the current
throughput** for this workload.

All 36 final boolean gates passed. The optimized JIT, same-binary interpreter,
and previous JIT produced byte-identical USB console output. Each completed
9,820,325,756 guest instructions, 47.7508515 simulated seconds and 101 binary
output messages. The firmware still hardcodes `ssaa_receipt=yellow` for its
separate anti-aliasing validation status; these runs do not resolve that status.

## Implementation and limits

Compiled blocks now own instruction storage independently of the decoded arena.
After decoded entries are discarded, recently used compiled blocks can be
reused if the PC, block length, fast-memory contract and every decoded
instruction field match. Modified code and different observer boundaries cannot
reuse an incompatible block. Old blocks expire after two unused decoder
generations; each arena flush retains at most 16,384 blocks and 64 MiB of emitted
WASM per core. New allocations can exceed those retention limits between flushes.

The common whole-block path no longer tests entry position and instruction budget
for every instruction. Cuts and resumptions retain their checked path. Generated
code loads only the block's operand registers and calculates register-window
collision state once per entry. Memory helpers that immediately exit do not
reload and spill the register file again.

Peak live generated-module payload was **20.9 MiB**, across a peak of 2,945 live
modules. This is emitted WASM payload, not total browser memory: engine-generated
machine code, linear memory and other browser allocations are additional.

The scheduler and its instruction-count timing model are unchanged. These tests
establish agreement with the interpreter, not cycle accuracy against hardware.

## Evidence and reproduction

Inputs match the [previous comparison](../tinydraw-browser-jit-2026-09-04/README.md):
TinyDraw main `7a157d44a9da3312b1ecda2b45b116af2de28e63`, ESP-IDF v6.0.2,
604 tile slots, Waveshare AMOLED V2, 16 MiB flash and 8 MiB PSRAM. `inputs.json`
records the exact firmware and measured WASM hashes. `source-sha256.json` identifies
the working-tree source files over simulator base
`fdc30864a5d397c4215147ffa2ad1f8be10bce77`.

Both new measurements used an unpaced dedicated Web Worker in Chromium 152 on an
M1 Pro, with two-million-cycle slices, a fresh emulator, no stubs and no canvas
rendering. JIT ran first, then the interpreter; no other simulator run or build
ran concurrently. These are single runs, not a performance distribution or an
interactive drawing-latency measurement.

The result JSON files, console logs and event streams retain both runs.
`validation.txt` records 5,067 actual-WASM differential cases, five firmware smoke
manifests and WASM Clippy. New cases exercise retention, changed instructions,
eviction and cached collision checks across all 16 register-window bases, three
colliding frames, operand-window ranges, PS modes, budgets and entry positions.
The existing timer, script-stop and peer-state tests also pass. The native
execution backend was not changed by this optimization.

While the local artifacts remain, start
`python3 target/tinydraw-battery-jit-opt/serve.py`. Open
`http://127.0.0.1:8791/` for JIT or `http://127.0.0.1:8791/?jit=0` for the interpreter.
Run sequentially and archive the server's `browser-*` outputs between runs;
event and console files append. For a separate launcher, copy this directory,
copy `assets.example.json` to `assets.json`, set the paths to images matching
`inputs.json`, and run its `serve.py` from the repository root.
