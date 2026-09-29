# TinyDraw battery in browser WASM — 2026-09-04

Capture retention: summaries and compact results remain here. Full profiles, event
streams, console logs and screenshots are available through the [capture archive](../ARCHIVE.md).
For raw-file reproduction commands below, restore those files to their original paths first.

A fresh 604-slot gate firmware built from TinyDraw main
`7a157d44a9da3312b1ecda2b45b116af2de28e63` completed in a dedicated browser
Web Worker on an Apple M1 Pro. All 36 final boolean gates passed. The firmware
retained its separate `ssaa_receipt=yellow` annotation. No explicit `pass=0`,
watchdog, crash, or emulator stop was observed.

| Measurement | Result |
| --- | ---: |
| Browser execution wall time | 156.4615 s |
| Simulated time | 47.7508515 s |
| Simulated seconds per wall second | 0.3052 |
| Guest instructions | 9,820,325,756 |
| Guest instruction throughput | 62.765 million/s |
| Binary output messages | 101 |
| WASM JIT attempts | 5,730 |
| WASM JIT preparations / commits | 0 / 0 |

The simulator was unmodified main `fdc30864a5d397c4215147ffa2ad1f8be10bce77`,
built with `tools/wasm-build.sh`. No native simulator execution contributed to
these measurements. The driver uses the browser's prepare/run/commit JIT
handoff and 2-million-cycle interpreter fallback, without real-time pacing.
It drains binary output but does not render it to a canvas. Thus this measures
browser-worker execution, not interactive display/input performance.

This is a functional and throughput baseline under the existing approximate
scheduler timing. Passing guest timing gates does not establish cycle accuracy
or agreement with physical-board durations. This single run does not establish
a performance distribution. A previous diagnostic attempt using an old image
was stopped and is excluded.

## Inputs and retained evidence

- `inputs.json`: exact source/simulator revisions and SHA-256 of every image.
- `environment.json`: host and browser identity.
- `result.json`: final verdict, time, instruction count, and JIT counters.
- `console.txt`: complete USB console; UART mirror excluded to avoid duplicate lines.
- `events.jsonl`: progress, console, and result events.
- `run.mjs`: browser-compatible driver, accepting an image loader and event sink.
- `dependencies.lock`: dependencies resolved for this build.

Firmware used ESP-IDF v6.0.2 from TinyDraw's `.idf-version`, with
`-DPROJECT_VER=7a157d4 -DTINYDRAW_FIRMWARE_VARIANT=gate
-DTINYDRAW_VECTOR_V2_TILE_SLOTS=604`. The committed lock named IDF 6.1;
the build reconciled that entry to the pinned v6.0.2 in an isolated source copy.
The board was `waveshare-amoled18-v2`, with 16 MiB flash and 8 MiB octal PSRAM.
Flash began erased except for bootloader, partition table and application.
No hardware was flashed and no firmware functions were stubbed by the runner.

The local build and browser launcher remain under
`target/tinydraw-battery-2026-09-04/`. To rerun while those artifacts exist, start
`python3 target/tinydraw-battery-2026-09-04/serve.py` and open
`http://127.0.0.1:8791/`. Each page load starts a fresh run; the local event logs
append, so use separate output files for another measurement.
