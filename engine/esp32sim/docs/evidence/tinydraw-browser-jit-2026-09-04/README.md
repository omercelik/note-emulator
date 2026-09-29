# TinyDraw browser JIT comparison — 2026-09-04

Capture retention: summaries and compact results remain here. Full profiles, event
streams, console logs and screenshots are available through the [capture archive](../ARCHIVE.md).
For raw-file reproduction commands below, restore those files to their original paths first.

The scheduler-integrated WASM JIT runs the fresh TinyDraw battery correctly, but
this first implementation shows no meaningful throughput improvement. Both runs
passed all 36 final boolean gates, with byte-identical USB console output,
9,820,325,756 guest instructions and 47.7508515 simulated seconds. The separate
`ssaa_receipt=yellow` annotation is hardcoded in the firmware’s final report.
It records a separate anti-aliasing validation status, outside the 36 boolean
gates; this simulator run does not resolve that status.

| Browser-worker run | Wall time | Compiled guest instructions |
| --- | ---: | ---: |
| Previous main baseline | 156.4615 s | 0 |
| Updated binary, JIT disabled | 155.5012 s | 0 |
| Same updated binary, JIT enabled | 154.1016 s | 4,861,434,127 |

The JIT run handled 49.5% of guest instructions through compiled blocks,
including memory helpers. It compiled 77,119 modules with zero compilation
failures; synchronous host compilation took 5.4028 seconds. The released count
was 74,101 before emulator deletion, indicating substantial cache turnover.
These are single runs: the 0.9% difference between JIT and interpreter is too
small to establish a reliable speedup. The JIT run reached 0.310 simulated
seconds per wall second, still about 3.23 times slower than real time.

## What changed

Eligible integer, branch and memory blocks compile after 32 executions. They
share the emulator's memory and function table, and subsequent dispatch stays
inside WASM. JavaScript installs and releases compiled modules. Unsupported
blocks remain interpreted; guarded memory misses use the ordinary bus helper.
The decoded-block cache now survives run slices and invalidates when observer
boundaries change or code-page versions change.

The normal scheduler still handles both cores, timer deadlines, script actions
and console draining. This replaces the page's earlier external quantum
handoff. It uses the default instruction-count timing model; passing the guest's
timing gates does not establish cycle accuracy or agreement with physical-board
durations.

The next performance work should reduce generated-code overhead and cache
turnover before broadening compilation. The profile's largest functions include
masked chord-row painting (11.96%), the native-kernel gate (6.72%), PNG filtering
(5.80%), mask clearing (5.11%) and panel streaming (5.04%). `profile.txt` records
the full top 20; its separately instrumented run is not a throughput baseline.

## Reproduction and validation

Firmware is unchanged from the [fresh baseline](../tinydraw-browser-battery-2026-09-04/README.md):
TinyDraw main `7a157d44a9da3312b1ecda2b45b116af2de28e63`, ESP-IDF v6.0.2,
604 tile slots, Waveshare AMOLED V2, 16 MiB flash and 8 MiB PSRAM. Inputs and
SHA-256 values are in `inputs.json`; the simulator starts from main
`fdc30864a5d397c4215147ffa2ad1f8be10bce77` plus the working-tree changes identified
by `source-sha256.json`.

Both measurements used the same WASM file in Chromium 152 on an M1 Pro, a fresh
emulator per run, two-million-cycle slices, no real-time pacing, no stubs and
no canvas rendering. Both drained 101 binary output messages. This measures
worker execution rather than interactive drawing latency or display performance.
The JIT run preceded the interpreter run. Superseded development runs and an
interrupted interpreter run are excluded.

`jit-result.json`, `interpreter-result.json`, their console logs and event streams
retain the complete comparison. `validation.txt` records the checks: 167
release/golden tests, 2,762 actual-WASM differential cases, five firmware smoke
manifests, Clippy, and a browser UI boot. Differential cases include script-stop
console draining and compiled MMIO changes to the peer core with a timer deadline.

While local build artifacts remain, start
`python3 target/tinydraw-battery-jit/serve.py`, then open
`http://127.0.0.1:8791/` for JIT or `http://127.0.0.1:8791/?jit=0` for the interpreter.
Run them sequentially. Each load starts a fresh battery; archive the server's
`browser-*` outputs between runs because event and console logs append.
For a separate launcher, copy this directory, copy `assets.example.json` to
`assets.json`, set its paths to the images matching `inputs.json`, and run its
`serve.py` from the repository root.
