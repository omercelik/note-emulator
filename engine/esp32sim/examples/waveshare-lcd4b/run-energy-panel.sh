#!/bin/sh
# Run the esp32-screen energy/Home-Assistant panel (Waveshare ESP32-S3-Touch-LCD-4B) in the emulator.
#   ./run-energy-panel.sh [--web 8768] [extra emulator flags...]
#   ./run-energy-panel.sh --wifi "ssid=NAME,psk=PASS"      # live prices + Home Assistant
# Without a --wifi spec there is no network to join, so esp_wifi_start is stubbed: otherwise the WiFi
# blob spins in PHY calibration on core 0 and starves the LVGL task. Pass --wifi and the real stack
# runs — association, DHCP, NTP, HTTPS (docs/networking-howto.md).
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
FW=${ENERGY_PANEL_BUILD:-$HOME/work/ai-smarthome/esp32-screen/build}
STUB="--stub esp_wifi_start=0"
for a in "$@"; do [ "$a" = "--wifi" ] && STUB=""; done
exec "$HERE/../../target/release/esp32sim" --board waveshare-lcd4b --boot rom --flash-mb 16 --psram-mb 8 \
  --bootloader "$FW/bootloader/bootloader.bin" --ptable "$FW/partition_table/partition-table.bin" --app "$FW/energy_panel.bin" \
  --elf "$FW/energy_panel.elf" $STUB --console usb --no-dump "$@"
