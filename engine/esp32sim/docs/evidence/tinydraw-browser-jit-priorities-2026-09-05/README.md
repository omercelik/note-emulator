# Highest-value browser JIT work for TinyDraw

Capture retention: summaries and compact results remain here. Full profiles, event
streams, console logs and screenshots are available through the [capture archive](../ARCHIVE.md).
For raw-file reproduction commands below, restore those files to their original paths first.

The strongest next investment is **compiling complete hot raster paths**: fill the
function-entry and variable-shift gaps, then add the native floating-point operations
those paths use. The larger architectural opportunity is keeping connected hot paths
and Xtensa hardware loops inside generated WASM. The tested self-branch-only design
covered too little work and has been removed.

This ranking uses the current TinyDraw source, its device optimization receipts, a
fresh browser battery CPU/coverage profile, and a separate interactive drawing CPU
profile. It ranks opportunities; it does not claim a measured speedup for an
unimplemented compiler change.

| Priority | Work | Evidence and expected value | Main uncertainty / acceptance test |
| --- | --- | --- | --- |
| First bounded change | `Entry`, `Sll`, `Srl` coverage | These missing operations alone keep blocks representing 5.18% of sampled guest instructions interpreted. They include the mask-window routine used by rasterization. Modest compiler scope relative to a trace backend. | `Entry` rotates the register window; cached operands and window checks must be refreshed correctly. Measure newly compiled work and ordinary-build wall time. |
| Main coverage investment | Scalar floating-point JIT for raster paths | Blocks containing missing FP operations represent 19.08% of sampled guest instructions. Leading interpreted functions calculate row spans, pixel coverage, tapered strokes and curve geometry. | This is guest frequency, not CPU time or a 19% speedup promise. Some blocks have additional blockers. Preserve fused arithmetic, conversions, NaNs and disabled-coprocessor behavior; keep precise helpers where needed. |
| Largest structural opportunity | Budgeted connected blocks and hardware loops | About 1.964 billion block calls retire 9.820 billion instructions: roughly five per call. The dispatcher/interpreted-loop and machine-wrapper categories together occupy 37.6% of this diagnostic CPU sample. | That bucket includes interpreter work, so it is not all removable dispatch. Start with a measured hot path; retain timer, MMIO, invalidation, observer and peer-core boundaries. |
| Targeted follow-up | Repeated memory guards and code-version updates | The hottest generated functions include masked chord painting and panel pixel transport. Each emitted access performs its own address/TLB checks; every direct store updates a page version. | Guard cost is not separately measured. Hoist only when the whole range, aliasing and mapping lifetime are proven. Benchmark before building a general memory optimizer. |
| Later / workload-specific | PIE SIMD and more compilation-cache tuning | PIE plus selected 128-bit helpers account for 4.3% of the battery CPU sample. Host module compilation takes 0.92 s in a 154.94 s diagnostic run. | PIE can matter more on another workload. The compilation counter excludes Rust emission and later engine optimization; it does not prove all compilation costs are negligible. |

The first row is a small entry point into the second, not a substitute for it.
`Abs`/other integer coverage can be inexpensive, but the largest isolated missing
integer operation, `Muluh`, is concentrated in the battery's native-kernel test.
The arithmetic/shift bundle (`Abs`, `Sll`, `Srl`, `Sra`, `Muluh`, `Mulsh`, `Src`)
excludes `Entry` and could newly admit 4.47% of sampled instructions; only about
2.3 percentage points lie outside that test. This overlaps the first row's
`Entry`/`Sll`/`Srl` bundle: their union admits 8.92%, so the percentages are not additive. An aggregate battery gain
there should not be presented as an equivalent drawing improvement.

TinyDraw already removed the expensive floating-point library calls. Its
[device arithmetic receipt](https://github.com/aliceisjustplaying/tinydraw/blob/7a157d44a9da3312b1ecda2b45b116af2de28e63/benchmark-results/wave3-cold-compute/COLD_COMPUTE_CAMPAIGN_RECEIPT.md)
describes replacing per-row division, floor, ceil and square-root calls with native
arithmetic, and moving reciprocals out of repeated row work. The
[current rasterizer](https://github.com/aliceisjustplaying/tinydraw/blob/7a157d44a9da3312b1ecda2b45b116af2de28e63/vector_v2/src/incremental_rasterizer.cpp)
retains floating-point adds, multiplies and conversions intentionally. These are
exactly the kinds of operations absent from this WASM emitter. The simulator should
execute that optimized firmware efficiently; this profile does not establish a
reason to rewrite TinyDraw's arithmetic again.

A concrete structural target is the panel transport's block at `0x40383349`, the
largest individual named generated block in the fresh CPU profile. Disassembly shows
an Xtensa `loop` ending at `0x40383353` around its load/store pair. That active loop
uses the emitter's checked path and repeatedly returns through the dispatcher.
The rejected experiment admitted explicit branches back to the block head, excluded
stores, and did not accelerate active hardware loops. It therefore missed this
important kind of real loop. Supporting it requires budgeted loop execution and
code-invalidation handling, not merely adding `Loop` to an opcode list.

Drawing responsiveness also has an independent, concrete host-side opportunity.
The production worker yields after approximately 25 ms, checked between simulator
calls, and each call can run up to two million simulated cycles. In addition,
[the machine](../../../esp-soc/src/machine.rs) polls browser input and publishes
frames once per 20 ms of simulated time. Input waits in that queue until polling.
These waits can remain visible even after a CPU improvement. Test delivering queued
input at safe run boundaries and separating display publication from input polling,
with shorter host execution turns during interaction. Preserve guest device timing.

The drawing CPU profile covers three simple strokes and settling intervals. Its
hottest named generated functions are task scheduling, critical sections and I²C
touch handling, unlike the battery's raster-heavy profile. Of 12.027 sampled seconds,
4.603 are idle. This is not a saturation test or a dense-document drawing profile.
Keep separate acceptance measurements for battery throughput, simple movement and
lift, and dense drawing/pan/settling. Hardware receipts also distinguish compute
from the panel's wire and scanout limits; faster host execution does not remove
those guest-device limits.

The cycle-accuracy goal remains. Current measurements agree with the instruction-count
reference, not a silicon-validated clock. Future priced execution must consume the
cycle model's budgets and expose relevant memory/peripheral events. Increasing
scheduling quanta or bypassing observable accesses to improve a benchmark would not
establish the desired simulator.

The self-loop comparison completed three runs each in order off/on/on/off/off/on:

| Mode | Wall seconds | Median |
| --- | --- | ---: |
| Disabled | 120.5686, 124.7485, 126.9061 | 124.7485 s |
| Enabled | 126.4694, 126.1573, 127.5950 | 126.4694 s |

Runs overlap; there is no demonstrated gain. Enabled execution removed 4,008,375
backedges, about 0.2% of the approximately two billion baseline block calls. Every
run passed all 36 boolean gates with identical console SHA-256 and instruction count.
A misleading pilot pair (194.9 s disabled, 125.3 s enabled) is retained separately
and excluded from that conclusion. The experiment patch, source/input hashes and
raw results remain in [self-loop-experiment](self-loop-experiment/). Its patch is
relative to the preceding working tree with CPU-profile boundaries, before the new
PC names. The feature is absent from the current implementation.

The retained tooling adds guest-PC function names in diagnostic builds, joins those
samples to the exact app/ROM symbols, attributes resumed execution to its block head,
and records each movement point plus lift reporting. The final battery sampled
2,400,170 guest instructions, including 923,249 interpreted instructions. It passed
all 36 gates with 9,820,325,756 total guest instructions and the same console hash as
the comparison. The separate `ssaa_receipt=yellow` remains unresolved.

[Battery summaries](battery/summary.txt), [CPU categories](battery/cpu-summary.txt),
and [drawing CPU attribution](drawing-profile/summary.txt) retain the detailed ranking.
The capture archive links the full JSON receipts and importable `.cpuprofile` files;
the joining tool accepts downloaded compressed event files. These diagnostic builds
change Rust inlining and add sampling overhead; their wall times are not optimization
benchmarks. CPU captures ran separately from builds and other simulator runs.

The diagnostic emitter passed all 18,189 actual-WASM differential cases, including
budget cuts, timers, memory faults, windows and code invalidation. The removed chaining
experiment had also passed its expanded 22,811-case suite and the 168-test workspace
suite; those checks did not make it a performance win. Inputs and firmware revision
are pinned in [inputs.json](inputs.json). See the
[benchmark instructions](../../../tools/browser-benchmark/README.md) to reproduce.
Automated captures now default to separate headless Chrome on port 9228, creating no
visible window. Earlier profiles and the self-loop comparison used ordinary Chrome;
measurements across those modes should not be treated as a matched comparison.

The [uninstrumented headless drawing replay](production-drawing/drawing-response.json)
completed three visible strokes, three successful firmware commits and responses to
all 24 movement points. Movement-to-canvas ranged from 46.2–69.4 ms; pen-down values
were 64.7, 52.8 and 43.9 ms. Input spacing remained approximately 100 ms. Lift-to-console
commit reporting was 101.9, 77.9 and 74.7 ms; those values include console delivery.
Last canvas changes after lift occurred at 540.8, 416.3 and 401.4 ms and do not assert
final vector-authority equality. These are small proxy measurements, not optical
latencies or percentiles. The screenshot confirms placement of the new strokes;
the pre-existing upper-left multicolour patch is outside that assertion. The headless
process used Chrome 152.0.7977.77/V8 15.2.124.19, while the earlier captures used
Chrome 152.0.7977.65/V8 15.2.124.18. This baseline is not compared as a performance
treatment against the earlier profiles. A background-tab attempt was stopped before
replay because it was heavily throttled; headless captures select their page without
creating or focusing an OS window.
