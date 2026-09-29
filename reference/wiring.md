# NOTE board wiring

Both variants share one ESP32-S3 baseboard (16 MiB flash, 8 MiB octal PSRAM); only the panel
differs. Sources: **[C]** NOTE4C `xiaozhi-zectrix/.../zectrix-s3-epaper-4.2/config.h`,
**[E]** emini `firmware/main/home_battery.c`, **[4]** NOTE4 reference
`itopinion/zectrix-note4-epd-demo` (`docs/HARDWARE.md`, `zectrix_board_config.h`, `zectrix_epd.cc`).

| Function | GPIO / address | Polarity / notes | Source | Status |
|---|---|---|---|---|
| OK / BOOT button | GPIO0 | active low; boot strap | C, 4 | verified in source |
| UP button | GPIO39 | active low | C, 4 | verified in source |
| DOWN / power button | GPIO18 | active low; 3 s hold = shutdown (NOTE4). **Must idle high at boot** (ADR-009) | C, 4 | verified in source |
| Battery latch | GPIO17 | high keeps battery rail on | C (`VBAT_PWR_PIN`), 4 | verified in source |
| Battery voltage | GPIO4 = ADC1_CH3 | 1:2 divider, 12 dB atten, 12-bit | E | verified in source (emini driver) |
| Charging | GPIO2 | low = charging | C, 4, E | verified in source |
| Charge full | GPIO1 | NOTE4C: high = full [E]; NOTE4: polarity **unverified** | E, 4 | tracked |
| EPD power rail | GPIO6 | high = on | C, 4 | verified in source |
| EPD SPI | SPI3: SCLK12, MOSI13, CS11, DC10, RST9, BUSY8 | NOTE4 reads back on MOSI (3-wire, temp `0x40`) | C, 4 | verified in source |
| Status LED (green) | GPIO3 (both) | active low | C, 4 | verified in source: NOTE4C `zectrix-s3-epaper-4.2/board_power_bsp.cc` `PowerLedTask` (charge status, 500 ms blink). On the NOTE4C it shows through the rightmost hole of the speaker grille's middle row (hardware observation) |
| Audio rail | GPIO42 | active high | C, 4 | verified in source |
| Speaker PA | GPIO46 | high = enabled | C, 4 | verified in source |
| I2S | MCLK14, BCLK15, WS38, DOUT45, DIN16 | ES8311, 16 kHz | C, 4 | verified in source |
| I2C0 | SDA47, SCL48 | shared by RTC, codec, NFC | C, 4 | verified in source |
| RTC PCF8563 | 0x51, INT GPIO5 | factory firmware aborts if absent (probe 2026-09-27) | C, 4 | verified in source |
| Codec ES8311 | 0x18 (7-bit) | `ES8311_CODEC_DEFAULT_ADDR` | C | verified in source |
| NFC | 0x55, power GPIO21, field detect GPIO7 (active low) | modelled as absent (NACK + diagnostic) | C, 4 | deferred |

`VBAT_PWR_GPIO` (=18) in [C] is the button read in `Power_Init()`, not a latch; see ADR-009.
Board revision for both profiles: PCB 1.0. Re-verify on any new revision.
