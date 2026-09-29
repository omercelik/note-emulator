# TinyDraw browser JIT: hot call blocks and fewer instruction checks

Capture retention: summaries and compact results remain here. Full profiles, event
streams, console logs and screenshots are available through the [capture archive](../ARCHIVE.md).
For raw-file reproduction commands below, restore those files to their original paths first.

The final candidate reduced the median battery time from **138.4022 seconds** to
**132.3533 seconds** across three runs each: **4.37% less wall time**. All candidate
runs were faster than all baselines in this session.

| Build | Wall seconds, three runs | Median |
| --- | --- | ---: |
| Previous JIT | 138.3729, 138.5515, 138.4022 | 138.4022 s |
| Final candidate | 132.3533, 132.2757, 133.2033 | 132.3533 s |

The same-candidate-binary interpreter took **159.1223 seconds**. Every run passed
all **36 boolean gates**, produced byte-identical USB console output, and completed
9,820,325,756 guest instructions and 47.7508515 modeled seconds. The separate
hardcoded `ssaa_receipt=yellow` status remains unresolved.

Instructions executed through compiled blocks (including helpers) rose from
4,877,588,870 to 6,023,686,459: **49.7% to 61.3%**. Peak live emitted WASM payload
rose from **20.9 MiB to 22.6 MiB**; this excludes engine-generated machine code and
other browser memory. Compilation count rose from 7,516 to 10,634, with zero failures.

The original 130.0002-second result was a separate single run; these percentages
use the repeated baselines measured in this session. Three runs establish a local
comparison, not a broad performance distribution. The candidate reaches 0.361 times
the current instruction-count clock's rate, which is not hardware-validated real time.
Raw results and calculation inputs are in [comparison.json](comparison.json).

## What the browser profile found

The instrumented browser run sampled 2,403,770 instruction iterations. Of these,
1,212,131 (50.4%) ran through interpreted blocks. Blocks with a supported prefix
and only a final call or return missing from JIT coverage accounted for 278,961
sampled iterations: **11.6% of all sampled work**, or **23.0% of interpreted work**.

The largest such individual block was in the panel transport's `stream_rect`:
loads, bit operations and stores followed by `Call8`. A hot block in
`apply_masked_operation_chord_rows` had the same problem. See
[the profile summary](profile-summary.txt) for PCs, operation lists and symbols.

This is an instruction-frequency profile, not a wall-time breakdown. The optional
`jit-profile` build samples block calls pseudorandomly with probability 1/4096.
Its raw clock intervals include lookup and dispatch, including helpers inside
compiled execution, but exclude the outer SoC scheduler. Clock-call overhead and
quantization dominate these short intervals; their totals cannot reliably divide
elapsed time among the interpreter, JIT, memory helpers and scheduler. The
instrumented run's 148.7631 seconds is **not** an optimization benchmark.

## What changed

Normal `Call0/4/8/12` and indirect `Callx0/4/8/12` instructions now have generated
WASM implementations. Indirect targets are captured before writing the return
address, including when those registers alias. Window-overflow guards and the
interpreter's handling of illegal calls remain in place.

Supported prefixes ending in `Ret`, `RetN`, `Retw` or `RetwN` are also eligible
for compilation. Their final return uses the ordinary interpreter helper. Dirty
registers are saved before the helper; generated code then exits without reloading
or overwriting the helper's changes to registers or window state. Single-instruction
blocks remain interpreted, and calls/returns must be the final instruction.

A block-entry guard now selects the faster whole-block path only when there can
be no register-window collision and no active loop end inside the block. That
path omits the corresponding checks for each instruction. Budget cuts, resumed
blocks, active loop ends and possible window collisions retain the checked path.
The generated call/return work alone did not improve the repeated-run median;
the `direct-calls-only-*` receipts preserve that intermediate comparison.

The scheduler, memory model and instruction-count timing model are unchanged.
This work establishes agreement with the interpreter, not hardware cycle accuracy.

## Validation and reproducibility

[Validation details](validation.txt) cover 18,189 actual-WASM differential cases,
including 11,664 new call/return cases and 1,458 block-entry guard cases, all five
firmware smoke tests and WASM Clippy.
Normal comparison binaries contain no profiling clock import or sampling code.
After measurement, the root checkout also received the review fixes for three missing
console expectations and the legacy external API's halt/console handling. The rebuilt
root passed 167 native/workspace tests (including the golden-output suite), all 18,189
WASM differential cases, eight firmware smoke manifests and both Clippy checks.
`inputs.json` distinguishes the measured `after` binary from the subsequently rebuilt
`delivered` binary; `post-review-source-sha256.json` records the latter's source snapshot.

Firmware is the same fresh TinyDraw main build as the
[previous comparison](../tinydraw-browser-jit-opt-2026-09-04/README.md):
`7a157d44a9da3312b1ecda2b45b116af2de28e63`, ESP-IDF 6.0.2, 604 slots, Waveshare
AMOLED V2, 16 MiB flash and 8 MiB PSRAM. `inputs.json` identifies every binary by
SHA-256. `source-sha256.json` records the working-tree source snapshot.

Measurements use a fresh dedicated Web Worker per run in Chromium on the M1 Pro,
unpaced two-million-cycle slices, no stubs and no canvas rendering. No other
simulator runs or builds execute concurrently. This measures battery throughput,
not interactive drawing latency. The page uses cross-origin isolation for the
profiling clock; both baseline and candidate use the same page configuration.

Run `python3 target/tinydraw-battery-jit-profile/serve.py` while the local assets
remain. `/suite.html` executes the repeated comparison; `/` runs the instrumented
build. For a separate launcher, copy this directory and supply `assets.json`
with paths to the binaries identified in `inputs.json`. The archived suite runs
three baselines, three candidates and one interpreter. The final candidate measurements
followed the three baselines; intermediate experiments and builds occurred between
the baseline set and final candidate set, never during a timed run. Archive or clear old
receipt files before repeating: console/event logs append.

`profile-result.json` retains raw per-core profiles. To regenerate the symbol
summary, place the firmware's `xtensa-esp-elf-nm -n -C` output in `symbols.txt` and
run `python3 summarize.py`. The separately retained `helper-only-*` receipts
capture an earlier candidate that used helpers for calls as well as returns;
its single-run improvement was too small to establish a useful gain.

## Direction after this experiment

The profile recorded 1,962,831,458 block-execution calls for 9,820,325,756 guest
instructions: approximately five instructions per call. Each call passes through
block lookup, CPU accounting and the machine's block-dispatch wrapper. This makes
boundary overhead a plausible next target, but instruction counts do not establish
its share of elapsed time.

The next diagnostic should be a host CPU-time profile separating generated code,
interpreter execution, memory helpers and machine scheduling. For browser profiling,
Chrome recommends its Performance panel; attaching the source debugger can change
WASM optimization, so wall-time measurements should also remain separate from that
session. See [Chrome's WASM profiling guidance](https://developer.chrome.com/docs/devtools/wasm#profile-performance).

If the CPU profile supports the hypothesis, a bounded architecture experiment is to
execute several connected hot blocks while keeping guest registers in WASM locals,
within the existing 64-instruction scheduling quantum and its earlier exit conditions.
The experiment must preserve timer deadlines, interrupts, MMIO effects, code invalidation,
observer stops and peer-core visibility. It should demonstrate a substantial benefit
on one hot path before becoming a general compiler design.

Cycle accuracy remains a separate acceptance requirement. The default execution path
currently counts instructions; agreement with it is a functional/timing-regression
oracle, not validation against silicon. Browser execution can improve now while the
cycle model is refined, provided generated execution consumes the model's budgets
and preserves its observable boundaries. Hardware receipts must ultimately validate
those rules, including SRAM/PSRAM and peripheral behavior. The current modeled-time
ratio should not be read as a hardware-validated real-time result.
