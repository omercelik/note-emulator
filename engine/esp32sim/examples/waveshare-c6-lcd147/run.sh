#!/bin/sh
# Run the IEEE 802.15.4 energy scanner on the emulated Waveshare ESP32-C6-LCD-1.47.
#   ENERGY_SCAN_DIR=~/work/esp32/energy_scan examples/waveshare-c6-lcd147/run.sh [--max-seconds 8] [--tft-png lcd.png]
# The firmware: github.com/joakimeriksson/esp32 (energy_scan), `idf.py set-target esp32c6 build`.
set -e
cd "$(dirname "$0")/../.."
B="${ENERGY_SCAN_DIR:-$HOME/work/esp32/energy_scan}/build"
[ -f "$B/energy_scan.bin" ] || { echo "no $B/energy_scan.bin: set ENERGY_SCAN_DIR to the built energy_scan project" >&2; exit 2; }
[ -x target/release/esp32sim-c6 ] || cargo build --release -p esp32sim
exec ./target/release/esp32sim-c6 --boot rom --flash-mb 4 --board waveshare-c6-lcd147 \
    --bootloader "$B/bootloader/bootloader.bin" --ptable "$B/partition_table/partition-table.bin" \
    --app "$B/energy_scan.bin" --elf "$B/energy_scan.elf" \
    --stub bb_init=0 "$@"
