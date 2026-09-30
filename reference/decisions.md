# Decision log

Each record states the decision, why, and what would change it. Numbering matches
the implementation proposal's D1–D12 (approved 2026-09-27). Append new records; never
rewrite an old one — supersede it.

## ADR-001 — Adopt the rewrite spec
Implement `note4c/NOTE_Emulator_Rewrite_Plan.md`: Rust engine on esp32sim, SwiftUI app,
versioned local protocol, gates and test IDs.
**Why:** esp32sim runs the unmodified ESP-IDF Wi-Fi blob as a station; no QEMU fork can.

## ADR-002 — Parallel tracks with a replay backend
Protocol, storage, CLI and SwiftUI are built against `note-machine::replay` while engine
gates run. Replay results never satisfy a native gate.

## ADR-003 — Split G2 into G2a and G2b
G2a: inbound forwarding and the setup address. G2b: native SoftAP + emulated station peer,
AP+STA, vmnet Shared.

## ADR-004 — Developer workflows are requirements
DEV-01 GDB stub, DEV-02 flashing through ROM download mode, DEV-03 ELF symbolication,
DEV-04 core dump. Pass conditions are in the proposal §11 (G6).

## ADR-005 — Fork and own esp32sim
Vendored at `4ab7e900fee998d0137c2c57d59de9d05339d093` (2026-09-23) in `engine/esp32sim`.
Every change is listed in `engine/esp32sim/PATCHES.md` with its fixture. Upstreaming is optional.
**Why:** 1-month-old project with one dominant contributor; we carry SPI3/ADC/I2S-RX/AP work ourselves.

## ADR-006 — Clean-room device models
Rust peripheral and panel models are written from datasheets, ESP-IDF drivers and the MIT
NOTE4 reference driver. The GPL `qemu-note4c.patch` is behavioural evidence only; no code is
translated from it.

## ADR-007 — Personal tool first
Ad-hoc signing. Developer ID, notarization and public-distribution tests are optional gate G9.
Isolated-environment testing of the local bundle stays mandatory in G8.

## ADR-008 — Mac browser access for unmodified firmware is mandatory
NET-01 (station) and NET-02 (`http://192.168.4.1/` to a SoftAP guest). Legacy
`HOME_EMULATOR` OpenETH builds may be supported to unblock previews if G2b stalls; the native
G2b tests stay mandatory for G8 and for retiring the old runtime.

## ADR-009 — GPIO18 is an input at boot, not a second latch
Factory `Power_Init()` sets GPIO17 (latch) and then spins until `gpio_get_level(GPIO18)` is
high. GPIO18 is the DOWN/power button; the board model must idle it high (pull-up).
Evidence: `xiaozhi-zectrix/main/boards/zectrix/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc:204-217`.

## ADR-010 — Privileged helper owns port 80 on 192.168.4.1
Verified 2026-09-27 as the normal user: bind `127.0.0.1:80` and `:1023` → EACCES, `:1024` OK;
`0.0.0.0:80` succeeds but would expose the device on every interface, so it is rejected.
The helper adds the `/32` lo0 alias, binds `192.168.4.1:80` and passes the fd to `note-emu`
(`SCM_RIGHTS`). Because a passed fd survives the helper closing its copy, `note-emu` closes the
listener on revocation/disconnect/stop and acknowledges; the helper removes the alias only after
the acknowledgement or verified process exit, and reconciles leases after its own restart.

## ADR-011 — RFC2217 for flashing, PTY only for monitoring
Verified 2026-09-27: `TIOCMGET`/`TIOCMBIS` on both ends of a macOS PTY → ENOTTY, so DTR/RTS
auto-reset cannot work over a PTY. `note-emu --serial-rfc2217` serves RFC2217 on loopback;
DTR/RTS drive EN/GPIO0. Explicit `ndb boot download|normal` is the fallback. RFC2217 does not
reproduce USB-Serial/JTAG USB identity.

## ADR-012 — Mask ROM is imported, never bundled
*Superseded by ADR-018 (the ROM ships with the project).*
`ndb rom import <esp32s3_rev0_rom.elf>` verifies the SHA-256 against known esp-rom-elfs releases
and copies it to `~/Library/Application Support/NOTE Emulator/rom/`. The engine loads only from
there. Known: esp-rom-elfs 20241011 →
`c0ce0f338d1de1bdc6efbef1591779a2a42c1ab7d759d3c6ae8ae63a7dd34cfd`.

## ADR-013 — One SSD2683 controller model, two panel variants
Both NOTE4 and NOTE4C drive a Solomon SSD2683 over the same SPI3 wiring with the same command
set (the NOTE4C factory driver even names its packer `pack_1bpp_to_2683`; emini `home_panel.c`
uses the same OTP/`0x10`/`0x04`/`0x12`/`0x02` sequence). Profiles therefore say
`panel: ssd2683` plus `variant: bwry | mono`; the controller model is shared and only the
mapping from 2-bit RAM codes to visible pixels differs. Supersedes the proposal's separate
"Uc4ColorPanel"/"Ssd2683Panel". Register `0x00` (panel setting) is tracked: emini omits it,
the factory NOTE4C driver sends `2F 2E`, NOTE4 `2F 0E`/`2F 8E`.

## ADR-014 — A virtual access point is always present; host NAT is opt-in
esp32sim completes the Wi-Fi PHY IQ calibration only when a virtual AP exists
(`periph.rs`: "IQ estimation completes once there is an AP"); without one, unmodified NOTE4C
firmware spins in PHY init. Every NOTE machine therefore has the virtual AP and virtual subnet
(DHCP/DNS/NTP answered locally); traffic reaches the host network only when NAT is enabled
(`note-emu --nat`), so "networking disabled" still makes no host network access (Spec §8.8).

## ADR-015 — Flashing goes over USB-Serial/JTAG
NOTE boards wire USB-C straight to the S3's USB-Serial/JTAG (no USB-UART bridge), so
`note-emu --serial-rfc2217 PORT` carries that port by default; `--serial-port uart0` is kept for
other boards. With the engine's USB-Serial/JTAG reporting a connected host, the mask ROM's
download mode selects USB (`UartDev.buff_uart_no = 4`) and its UART0 RX interrupt handler then
faults on UART0 input — so UART0 download is a known limitation, not a supported path.
DTR/RTS follow the classic auto-reset circuit (RTS holds EN low, DTR holds GPIO0 low); an EN
release is a POWERON reset that re-applies the mask ROM image and straps `0x00` (download) or
`0x08` (SPI boot), matching what real boards print.

## ADR-016 — Shared networking (NET-03) is optional
Decided 2026-09-29. Shared mode would put a device on a real network through vmnet, so other
machines could reach it and LAN discovery (mDNS, UDP broadcast, SSDP) would work. None of the
firmware in use needs that: emini, Friday, the factory image, the NOTE4 demo and geminilive all
work with outbound NAT plus access from this Mac (a loopback forward, or `http://192.168.4.1/`
through the root helper). vmnet needs an Apple entitlement (or a packet relay in the root helper)
and would expose an emulated device on the user's LAN. NET-03 moves from the G2b and G8 exit
criteria to optional, alongside G9. `shared` stays a valid AVD network mode and reports
`PermissionDenied` until it is built; `network.info` keeps reporting discovery as unsupported.

## ADR-017 — A device window is the device
Decided 2026-09-29, after the app let devices run with no window, restored windows for devices
that had stopped, and opened duplicate windows. The app follows the Android Emulator:
- **Open** turns a device on (quick boot: resume where it left off) and shows its one window;
  for a running device it brings that window forward. A device window's identity is its
  instance's control socket.
- **Closing the window turns the device off**: flash committed, quick-boot snapshot saved.
  Quitting the app turns off every device it started, whichever windows are open.
- Devices started by `ndb` or `note-emu --avd` show as "Running outside the app"; closing their
  window only detaches it.
- Device and Controls windows are not restored after a relaunch. A device window closes when
  its instance goes away; a Controls window follows its device (live or stopped view).
- Reset has three levels: **Restart** (chip reset, flash kept), **Cold Boot** (fresh start,
  flash kept, state still saved at stop: `note-emu --cold-boot`), **Erase Content and
  Settings** (`ndb avd wipe`: flash back to the imported image, quick-boot state dropped;
  named snapshots stay).
- `note-emu --avd` publishes its instance only once its control socket accepts connections,
  and survives its console reader going away (its output passes through an internal pipe).

## ADR-018 — The mask ROM and the NOTE4 demo ship with the project
Decided 2026-09-29; supersedes ADR-012. Both may be redistributed: Espressif publishes the
esp-rom-elfs ROM ELF files under Apache-2.0 and states that the licence applies to the compiled
files; the NOTE4 reference demo is MIT (Zectrix Lab). `third_party/` holds
`esp-rom-elfs/esp32s3_rev0_rom.elf` (release 20241011) and
`zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin`, each with its licence; the app bundle
carries them in `Resources/rom/` and `Resources/samples/`. The ROM is still hash-checked and loaded
only from the data directory: a missing ROM is imported from the bundled copy (`rom::bundled()`,
found next to the running binary, with no build-time path compiled in). `ndb rom import FILE`
still accepts another verified copy. An empty device list offers the demo.

## ADR-019 — The helper also leases the station address 10.0.2.15
Firmware that finishes setup joins the virtual AP and shows its station address, `10.0.2.15`,
as the way to reach it. That address is private to the emulator, so it did not open on the Mac.
In Setup networking for a NOTE4C, `note-emu` now takes a second lease from the helper for the
`10.0.2.15/32` lo0 alias and port 80, forwarded to the guest station's port 80. Each address is
its own lease with the ADR-010 rules (conflict checks, journal, reconcile, runtime-driven close).
The station lease is optional: a Mac network or VPN on `10.0.2.0` refuses only that lease, and
setup stays available. Only one running device can hold each address. The helper reports a lease
protocol version (`note-net-helper check`). Controls ▸ Network shows whether it is installed and
current, and its Install/Update button is the only place the administrator prompt appears. A
device whose helper is missing or outdated still starts: `note-emu` falls back to the loopback
address and reports `helper-unavailable` in `network.info` `permission`. The station address is a
per-device setting (`station_address` in `config.json`, `ndb avd station-address`), on by
default with the setup address.

