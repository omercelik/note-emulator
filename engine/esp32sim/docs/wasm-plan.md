# Plan: esp32sim in the browser (WebAssembly)

**Status (2026-08-28): built — phases 1–3 and 5 are done, see [wasm.md](wasm.md).** hello_world,
the Touch-LCD-4B panel with the SID player, and the Atech board all run at real time in a tab
(~62 Minsn/s from the block interpreter under V8, better than the 47 % estimate below because
the block interpreter landed after the spike was measured). What took a day rather than the
estimated 3–5: `esp32s3` compiled for wasm32 unchanged; the only runtime hazards were
`Instant`/`SystemTime` (a `host` module) and the `WebServer` snapshot for late-joining clients
(queue mode sends the board announcement itself). Open: phase 4, the network relay.

Today the emulator is a native binary that *serves* a browser UI: `--web PORT` starts a
dependency-free HTTP/WebSocket server, `web/index.html` draws the framebuffer on a canvas, plays
audio through WebAudio and sends clicks back. The CPU, peripherals, NAT sockets and file loading
all run natively.

This plan is about moving the emulator itself into the page, so a link is enough — no build, no
install, no local binary. Good for showing the panel or the Atech board to someone, for teaching,
and for regression demos that outlive a laptop.

## What the code says about feasibility

Measured on the tree at `02b24af`:

- **`xtensa-lx7` uses no host APIs at all** — no `std::thread`, `std::net`, `std::fs`,
  `Instant`, `SystemTime`, `std::env`. The decoder, interpreter, register windows, PIE and
  disassembler compile to wasm as they are. That is the half that matters.
- In `esp32s3`, host use is confined and shallow: `board.rs`, `crypto.rs`, `elf.rs`, `image.rs`
  and `lib.rs` are already clean; the rest is
  | file | what is host-bound |
  | --- | --- |
  | `web.rs` | the HTTP/WS server: threads + `TcpListener` — **not needed in wasm**, the page *is* the UI |
  | `nat.rs` | `TcpStream`/`UdpSocket` + a connect thread — needs a relay (see Networking) |
  | `machine.rs` | real-time pacing (`Instant`, `thread::sleep`), WAV/PNG/regtrace writing |
  | `net.rs` | one `SystemTime::now()` for the SNTP answer |
  | `periph.rs`, `bus.rs`, `wifi.rs`, `i2c.rs` | `std::env::var` debug switches (14 of them) |
  | `picture.rs` | `fs::read` for a camera image |
- **The UI seam is one line.** `web/index.html` is 237 lines with a single `new WebSocket` and a
  single `onmessage`; the message formats are already a clean protocol (web-ui.md). Swapping the
  transport for calls into a wasm module leaves the rest of the page alone.
- **The scheduler is single-threaded** — both emulated cores are interleaved in one loop, so
  nothing needs threads, `SharedArrayBuffer` or cross-origin isolation.

## Measured: what it will cost in speed

A spike compiled the interpreter both ways and ran the same instruction loop (`l32i.n`, `add.n`,
`addi.n`, `j`) 200 M times:

| build | throughput |
| --- | --- |
| native (aarch64, `lto = "fat"`) | 217 Minsn/s |
| wasm32-unknown-unknown under V8 (node 26) | 103 Minsn/s |

**wasm runs at ~47 % of native**, and the module is 97 KB. Applying that to today's full-emulator
numbers (which carry SoC ticks, interrupt checks and DMA on top of the interpreter):

| workload | demand | native today | wasm estimate |
| --- | --- | --- | --- |
| panel UI only | ~31 Minsn/s | 2.1× real time | ~1.0× — usable |
| panel + WiFi + HTTPS | ~47 Minsn/s | 2.1× | ~0.9× — usable, occasional lag |
| panel + SID player | ~89 Minsn/s | 0.99× | ~0.45× — audio will not hold |
| Atech 14-port board | ~31 Minsn/s | ~1.6× | ~0.8× |

So: the UI, WiFi and Home Assistant demos work in a browser; the SID player does not, until the
basic-block interpreter (roadmap item 4) lands. That is the honest trade, and it is worth saying
on the page itself rather than letting a visitor conclude the emulator is slow.

## Architecture

```
 index.html  (unchanged UI: canvas, WebAudio, touch, console tabs)
     |  postMessage, same message shapes as the WebSocket protocol
 worker.js   (JS glue, hand-written)
     |  direct calls + typed-array views into the module's memory
 esp32sim.wasm  (Machine: CPU cores, peripherals, boards, virtual AP, subnet)
```

Decisions:

- **No wasm-bindgen.** The repo has no third-party dependencies and the interface is small enough
  to hand-write: a dozen `extern "C"` exports plus `WebAssembly.Memory` views. Keeping that rule
  is worth more than the convenience. If the glue turns out to need JS objects (it should not),
  reconsider then.
- **Run in a Web Worker**, so a slow frame never freezes the page and the run loop can be a plain
  `while` bounded by a cycle budget rather than fighting `requestAnimationFrame`.
- **Zero-copy where it counts**: the framebuffer and the audio ring live in wasm memory; the worker
  hands the main thread `Uint8ClampedArray` views for `putImageData` and an `AudioWorklet` reads
  samples out of the ring. No per-frame serialisation, which is what the WebSocket path pays today.

Export surface (first cut):

```
new_machine(board: u32, flash_mb: u32, psram_mb: u32) -> handle
load_blob(handle, kind: u32, ptr: *mut u8, len: u32)   // rom / bootloader / ptable / app / elf
reset(handle)
run_cycles(handle, cycles: u64) -> u32                 // returns a stop reason
framebuffer(handle) -> *const u8, fb_len(handle), fb_version(handle)
audio_take(handle, ptr: *mut i16, max: u32) -> u32
touch(handle, x: u32, y: u32, down: u32), button(handle, id: u32, down: u32), knob(handle, delta: i32)
console_take(handle, ptr: *mut u8, max: u32) -> u32
stats(handle) -> *const u8                             // a small fixed struct, read by the glue
```

## Networking without sockets

More survives than one would expect, because the emulated network is pure Rust:

- **Works unchanged**: the virtual AP (`wifi.rs`), the WPA2 handshake, the crypto accelerators, and
  the whole emulated subnet in `net.rs` — DHCP, ARP, ICMP, the DNS responder, SNTP (with the clock
  coming from `Date.now()` instead of `SystemTime`). Firmware associates and gets an IP in the tab.
- **Needs a relay**: `nat.rs`. Options, in order of preference:
  1. **WebSocket relay** to a tiny host helper (`esp32sim --net-relay PORT`) that opens the real
     sockets. Same NAT code, transport swapped — reuse `nat.rs` verbatim behind a trait.
  2. **`fetch()` for HTTP(S) only**: terminate TCP in the emulator and turn a complete HTTP request
     into a `fetch`. Works for the price API, breaks on anything not HTTP and on CORS.
  3. **Nothing** — ship the `--net none` behaviour: names resolve to the emulated resolver and
     connections are refused fast. Fine for UI demos.
  Phase the relay last; (3) is the default for a public page.

## Phases

1. **Make the core `cfg`-clean** (~half a day). Put the host-bound pieces behind
   `#[cfg(not(target_arch = "wasm32"))]`; replace the 14 `ESP_EMU_DEBUG_*` env lookups with a
   `DebugFlags` struct set once at startup (the CLI reads the env, the page gets checkboxes — and
   it removes `std::env::var` calls from paths that run per operation); introduce a `host` module
   with `now_ms()` and `unix_time()`, native and wasm implementations. Checkpoint:
   `cargo build --target wasm32-unknown-unknown -p esp32s3` succeeds.
2. **Glue and page** (1–2 days). The exports above, `web/worker.js`, and a loader that takes the
   ROM/bootloader/app from `<input type="file">` or `fetch()`. Swap `index.html`'s transport;
   everything else in the page stays. Checkpoint: hello_world boots in a tab and prints on the
   console panel.
3. **Boards, audio, input** (1 day). Framebuffer view for the LCD boards, the audio ring +
   AudioWorklet, touch/buttons/knob, WAV and PNG "downloads" via Blob. Checkpoint: the Touch-LCD-4B
   panel renders and responds in the browser; the Atech board's ST7735 and ring work.
4. **Networking relay** (1 day, optional). Trait behind `nat.rs`, WebSocket transport, the
   `--net-relay` helper mode in the CLI.
5. **Publish** (half a day). A static page; `--web` keeps working for native runs. Add a build
   script and a CI job that at least compiles the wasm target so it cannot silently rot.

## Risks and things to decide early

- **Do not ship blobs.** The mask ROM ELF is Espressif's and the firmware images are the user's or
  Atech's. The page must load them from the visitor's own disk; a public demo needs a firmware whose
  redistribution is clearly ours to grant (hello_world built from IDF, or the Atech firmware only
  with permission). This decides whether the page is public or a local file.
- **Memory**: 16 MB flash + 8 MB PSRAM + SRAM + ROM ≈ 25 MB of wasm memory, plus allocation
  headroom. Fine, but set an explicit maximum and fail loudly rather than growing forever.
- **Audio is the first thing to break** when the emulator falls behind; the existing adaptive
  buffer logic in the UI helps, but the SID page will still stutter until the interpreter is faster.
- **Two run loops to keep in sync**: `Machine::run` currently paces itself with `Instant` and
  `thread::sleep`. In wasm the worker drives it with a cycle budget per tick instead — keep the
  pacing decision in one place so the two do not drift apart.
- **Don't fork the UI.** If `index.html` grows a native branch and a wasm branch, both will rot.
  The transport swap must be the only difference.

## Out of scope

A JIT or wasm-level code generation, threads/SharedArrayBuffer, and running the *native* build's
file-writing features unchanged. Those are not needed to make the browser build useful.
