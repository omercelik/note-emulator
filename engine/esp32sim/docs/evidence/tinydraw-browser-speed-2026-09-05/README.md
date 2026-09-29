# Faster browser drawing workloads

The combined scalar floating-point compiler, hardware-loop retention and input changes
reduced median TinyDraw workload time from **124.577 to 107.368 seconds:
13.81% less host wall time**. Every combined run was faster than every
baseline run in this local comparison.

TinyDraw is drawing firmware for the ESP32-S3. Its automated workload exercises rendering,
panning, cache reuse, export, undo/redo and settling after drawing. These measurements use
ordinary release builds in headless Chrome, without a CPU profiler or diagnostic features.
These workload timings measure throughput; the separate replay below measures input response.
Neither measures physical-device timing.

| Build | Three runs, seconds | Median, seconds |
| --- | --- | ---: |
| Baseline | 125.976, 124.577, 123.234 | 124.577 |
| Combined stack | 105.547, 107.368, 107.599 | 107.368 |

Runs were serial in baseline/combined/combined/baseline/baseline/combined order, with
other simulator runs and builds stopped. Three runs per build establish this local
comparison, not a statistical confidence guarantee or a gain on every host/workload.

## Workload intervals

| Interval | Baseline median, s | Combined median, s | Less wall time |
| --- | ---: | ---: | ---: |
| Boot and native kernels | 21.596 | 21.063 | 2.47% |
| Cold rendering and initial pan | 14.976 | 11.924 | 20.38% |
| Pan sequences | 4.657 | 3.625 | 22.16% |
| Cache tour | 8.147 | 6.113 | 24.97% |
| Mixed drawing | 25.509 | 22.081 | 13.44% |
| Hairlines | 20.022 | 16.662 | 16.78% |
| Export | 11.986 | 11.601 | 3.21% |
| History | 10.907 | 8.572 | 21.40% |
| Settling | 5.839 | 4.695 | 19.59% |

These are intervals between firmware console milestones, including setup and console
delivery between those milestones. They are not isolated function timings.

All six runs passed the same **36 boolean firmware assertions**, produced byte-identical
USB console output and executed **9,820,325,756 guest instructions**. Instructions retired
through compiled blocks, including helper execution, rose from **66.55% to 85.98%**.
There were zero generated-module compilation failures.

The firmware separately prints a fixed `ssaa_receipt=yellow` marker for smoothing stroke
edges during settling. The existing settling-performance follow-up remains open; it is
separate from the 36 assertions above.

Cumulative generated WASM bytes increased from **74.11 to 87.54 MB**, and peak live
generated WASM bytes from **28.81 to 33.85 MB**. These byte counters describe generated
modules, not the browser process's total memory usage.

## Drawing response

Three fresh replays per build used the production worker, for nine strokes and 72
movement points per build. All strokes committed and all movement points were observed.
Each value below is the median of the three per-run medians.

| Endpoint | Baseline, ms | Combined, ms |
| --- | ---: | ---: |
| Pen-down to canvas | 58.590 | 41.405 |
| Movement to canvas | 59.700 | 44.805 |
| Lift to commit report | 53.585 | 39.860 |
| Lift to last canvas change | 409.355 | 439.225 |

Movement response improved by **24.95% at the median**, but one combined sample took
**492.820 ms**, versus a baseline maximum of **73.005 ms**. The trace does not establish
its cause; no worst-case latency improvement is claimed. Median startup to interactive
readiness fell from **137.232 to 120.536 seconds**.

The endpoint is changed pixels near the queued input position submitted to canvas,
not optical display. Other changes nearby can affect attribution. Lift-to-last-change
is retained as an observation, not proof that drawing has finished. See the
[compact replay comparison](drawing-comparison.json) for every timing sample.

## Implementation and validation

- **Scalar floating point:** compile arithmetic, comparisons, conversions, register moves,
  boolean branches and floating-point memory accesses. A small helper preserves fused
  multiply-add rounding. Whole blocks check coprocessor enablement once when its state
  cannot change; partial execution retains faults at the original instruction boundary.
- **Hardware loops:** retain eligible integer/load-store prefixes inside generated WASM,
  within the existing scheduler budget and timer cuts. A shared guard checks code versions
  before repeating. Block observers disable repetition; slow memory, faults and control-flow
  exits retain normal scheduler handling.
- **Input:** accept queued browser input at run entry without advancing device time.
  For 250 ms after input, the worker targets 4 ms execution slices and 8 ms turns; otherwise
  it keeps the original 2M-cycle slices and 25 ms turns. Synchronous WASM calls can overshoot
  these targets during expensive work. Queued knob input preserves pending scripted actions.

Validation passed for the combined runtime:

- 38,680 generated-WASM differential cases, with 36,360 compiled modules released.
- 172 workspace tests, including golden firmware outputs and observer checks; seven external
  tests were excluded. No tests failed or remained ignored in that run.
- Workspace Clippy, worker pacing/integration tests, the ordinary release build, and real
  WASM execution of the hello, C3 hello, C6 hello, C6 energy-scan and panel manifests.

A separate diagnostic build of the loop layer recorded **238,236,441 retained backedges** in the real
firmware, including **48,134,257** at the panel transport loop at `0x40383349`. The counter
excludes a final backedge that returns to the scheduler. Its [compact counter summary](loop-diagnostic.json)
is separate from production timings; this diagnostic run overlapped validation work.

## Reproduce and inspect

The measured combined runtime is commit `8df2f0ad35bdee878c7a3f24e25e66436911a79b`.
The baseline is `9dedf6959c658b85a14d938a691ecc82c40b3076`, which combines the earlier browser
JIT, benchmark, function-entry/shift and display fixes. Exact firmware, ROM, WASM and worker
hashes are in [combined inputs](combined-inputs.json) and [baseline inputs](baseline-inputs.json).
[Environment and capture order](environment.json) records the compiler and runtime versions.

The TinyDraw firmware and ROM are external inputs; fresh captures require binaries matching
those hashes. Follow the [browser benchmark instructions](../../../tools/browser-benchmark/README.md)
to build, select matching assets and worker code, and capture fresh runs.

The [comparison JSON](battery-comparison.json) retains all six timing samples, interval
measurements, console hashes, instruction counts and browser versions. The
[validation summary](validation.json) retains commands and outcomes. Raw captures,
profiler output, console streams and full logs remain in local ignored output directories.
