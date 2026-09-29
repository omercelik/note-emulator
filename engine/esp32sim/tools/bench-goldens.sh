#!/bin/sh
# A/B the three golden scenarios between two emulator builds, interleaved (tools/bench.py):
#   tools/bench-goldens.sh /path/to/base-esp32sim [/path/to/new-esp32sim] [rounds]
# The instruction counts must agree between the binaries; wall time is the number under test.
set -e
HERE=$(cd "$(dirname "$0")/.." && pwd)
BASE=${1:?base binary}; NEW=${2:-$HERE/target/release/esp32sim}; ROUNDS=${3:-5}
FW=$HERE/web/wasm/fw/public
ROM=${ESP32SIM_ROM:-$(ls "$HOME"/.espressif/tools/esp-rom-elfs/*/esp32s3_rev0_rom.elf | tail -1)}
ATECH="--rom $ROM --board atech14 --boot rom --no-dump --console none --bootloader $FW/atech-bootloader.bin --ptable $FW/atech-ptable.bin --app $FW/atech-firmware.bin"
echo "### atech script1 (5 s)"
python3 "$HERE/tools/bench.py" --rounds "$ROUNDS" --label base="$BASE" --label new="$NEW" -- $ATECH --script "$FW/atech-script1.txt" --max-seconds 5 2>/dev/null
echo "### sid jukebox (6 s)"
python3 "$HERE/tools/bench.py" --rounds "$ROUNDS" --label base="$BASE" --label new="$NEW" -- $ATECH --script "$FW/atech-sid.txt" --max-seconds 6 2>/dev/null
echo "### panel sid (7 s)"
python3 "$HERE/tools/bench.py" --rounds "$ROUNDS" --label base="$BASE" --label new="$NEW" -- --rom "$ROM" --board waveshare-lcd4b --boot rom --no-dump --console none --flash-mb 16 --psram-mb 8 \
  --bootloader "$FW/panel-bootloader.bin" --ptable "$FW/panel-ptable.bin" --app "$FW/panel-demo.bin" --flash-at "0x610000=$FW/energydata.json" --script "$FW/panel-sid.txt" --max-seconds 7 2>/dev/null
