# Licences

This project is MIT licensed (`../LICENSE`). Third-party parts:

| Component | Licence | Where |
|---|---|---|
| esp32sim (vendored engine) | MIT | `engine/esp32sim/LICENSE`, copy in `esp32sim-MIT.txt` |
| NOTE4 reference driver: SSD2683 command behaviour, and the vendor four-gray waveform table copied into `crates/note-machine/src/gray16_vendor.bin` | MIT, © 2026 Zectrix Lab | github.com/itopinion/zectrix-note4-epd-demo; licence text in `zectrix-note4-epd-demo-MIT.txt` |
| NOTE4 reference demo firmware `zectrix-note4-epd-demo-v1.0.0.bin` | MIT, © 2026 Zectrix Lab | `third_party/zectrix-note4-epd-demo/` with its `LICENSE` |
| Skin `Profiles/skins/note-shell.svg` | original vector drawing of the device shell, traced to measurements of Zectrix product photos supplied by the user | ours; the enclosure design is Zectrix's — redistribution reviewed in G9 |
| ESP32-S3 mask ROM `esp32s3_rev0_rom.elf` (esp-rom-elfs 20241011) | Apache-2.0, © Espressif Systems | `third_party/esp-rom-elfs/` with its `LICENSE`; the ROM code itself is not open source, the ELF files are licensed for redistribution |
| NOTE4C emini Home v0.6.2 | MIT for its own firmware, with permissively licensed components and fonts | `third_party/emini-home/LICENSE`, `THIRD_PARTY_NOTICES.md`, and `licenses/`; notices ship with the app |
| ESP-IDF 6.0 bootloader used in the emini sample | Apache-2.0 and component-specific licenses | built from `fixtures/diag-fw/panel`; notices in `third_party/emini-home/` |
| App icon `macos/Resources/AppIcon.svg` | original artwork (an e-paper device, not a Zectrix mark) | ours, MIT |

The GPL-2.0 `qemu-note4c.patch` in the old project is behavioural evidence only (ADR-006).
