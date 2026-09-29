# Browser JIT function entry and variable shifts

Capture retention: summaries and compact results remain here. Full profiles, event
streams, console logs and screenshots are available through the [capture archive](../ARCHIVE.md).
For raw-file reproduction commands below, restore those files to their original paths first.

Adding `Entry`, `Sll` and `Srl` reduced median TinyDraw battery time from
**125.9141 to 123.8853 seconds: 1.61% less wall time** across three runs each.
Every candidate was faster than every baseline in this local comparison.
This is a modest battery-throughput gain; it does not establish a drawing-latency
improvement or hardware cycle accuracy.

| Build | Wall seconds | Median |
| --- | --- | ---: |
| Baseline | 126.1262, 125.6042, 125.9141 | 125.9141 s |
| Entry and variable shifts | 123.9296, 123.8853, 123.5134 | 123.8853 s |

All six runs passed **36 boolean gates**, produced byte-identical USB console
output, and executed **9,820,325,756 guest instructions**. Instructions executed
through compiled blocks rose from 6,023,686,459 to 6,534,998,429, approximately
**61.34% to 66.55%**. These counters include helper execution.

The firmware also prints a fixed `ssaa_receipt=yellow` marker for settled
anti-aliasing: smoothing stroke edges after drawing. This is separate from the
36 boolean assertions. The existing settling-performance follow-up remains open;
closing it requires a dedicated settling workload, which this change does not provide.

## Implementation and correctness

`Entry` implements function entry by saving dirty registers in the old window,
rotating the register window, reloading cached operands and collision state, and
writing the adjusted stack pointer in the new window. Illegal entry instructions
retain interpreter exception handling. Instructions after an entry perform fresh
overflow checks even on the whole-block path: its initial window proof can become
invalid after rotation.

`Sll` and `Srl` emit variable shifts with explicit zero results when the effective
count is at least 32. Left shifts derive that count as `(32 - SAR) & 63`, matching
the interpreter. WASM's own masked shift counts alone would produce wrong results.

The actual-WASM differential suite passed **32,561 cases**. Added cases cover
shift counts 0–64, 127 and the maximum unsigned value; source/destination aliases;
window wraparound; every call increment; disabled windowing; exception mode;
illegal entry operands; dirty registers before entry; repeated entries; overflow
that becomes possible only after rotation; loop ends; and budget cuts/resumes.
Existing timer, memory, invalidation, observer-stop and peer-core cases also passed.
WASM Clippy passed with `cpu-profile,jit-profile,jit-tests` and warnings denied.

The production-worker drawing replay passed the 36 boot gates, committed all three
strokes, and responded to all 24 movement points. The
[screenshot](https://github.com/aliceisjustplaying/esp32sim/blob/c774a5b60144b5aee980a8f845ae7e8465088f4a/docs/evidence/tinydraw-browser-jit-entry-shifts-2026-09-05/drawing/drawing-after.png) confirms correct placement of the new strokes;
the pre-existing upper-left multicolour patch remains outside that assertion.
[The drawing receipt](drawing/drawing-response.json) records movement-to-canvas
values of 41.7–73.7 ms. This single smoke replay has no matched drawing baseline
and establishes neither improved latency nor final vector-authority equality.

## Measurement and reproduction

Runs used the same headless Chrome 152.0.7977.77 / V8 15.2.124.19 on the M1 Pro,
fresh workers, and ordinary release builds without CPU profiling or JIT sampling.
The order was baseline/candidate/candidate/baseline/baseline/candidate. No other
simulator runs or builds executed during measurement. The scheduler's existing
two-million-cycle host calls and instruction-count timing were retained.

Firmware is the September 4 TinyDraw build identified in the preceding
[priority report](../tinydraw-browser-jit-priorities-2026-09-05/README.md).
[Input hashes](inputs.json) identify the exact binaries;
[comparison data](comparison.json) records all runs and console hashes.
Each run directory retains its compact result; the capture archive links its event stream.
The baseline was rebuilt from the preserved dirty tree before this compiler edit.
The tested candidate was copied to `web/wasm/esp32sim.wasm`; the previous browser
binary is retained at `target/browser-entry-shifts/production-before.wasm`.

Use the [benchmark instructions](../../../tools/browser-benchmark/README.md) to
serve separate baseline and candidate asset maps, then run `capture-battery.mjs`
against each server's `/battery.html`. Retain the alternating run order and keep
profiling disabled. The source tree includes all preceding uncommitted display
and profiling work. Scalar floating-point coverage remains the next compiler
priority; no floating-point or connected-block optimization is included here.
