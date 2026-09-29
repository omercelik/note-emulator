#!/bin/sh
# Run the waveshare-autopling firmware (Waveshare ESP32-S3-CAM-OV5640) in the emulator with a picture
# on the camera. Headless: writes the speaker output to autopling.wav. Add --web 8767 for the live UI.
#   ./run-autopling.sh [picture.jpg|.png|.bmp|.ppm] [extra emulator flags...]
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
FW=${AUTOPLING_BUILD:-$HOME/work/ai-smarthome/esp32cam/waveshare-autopling/build}
PIC=${1:-$HERE/pedestrians.jpg}; [ $# -gt 0 ] && shift
case "$PIC" in *.bmp|*.ppm) BMP=$PIC ;; *) BMP=/tmp/esp32sim-cam.bmp; sips -s format bmp "$PIC" --out "$BMP" >/dev/null ;; esac
exec "$HERE/../../target/release/esp32sim" --board waveshare-cam --boot rom --flash-mb 16 --psram-mb 8 --cam-image "$BMP" \
  --bootloader "$FW/bootloader/bootloader.bin" --ptable "$FW/partition_table/partition-table.bin" --app "$FW/waveshare-autopling.bin" \
  --elf "$FW/waveshare-autopling.elf" --elf "$FW/bootloader/bootloader.elf" --console usb --wav "$HERE/autopling.wav" --no-dump "$@"
