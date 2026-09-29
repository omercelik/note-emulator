#!/bin/sh
# Differential test: single-step the real ESP32-S3 over its built-in USB-JTAG and compare
# with the emulator running the same flash image, efuses, strap and reset cause.
#   hw/difftest.sh [steps]          (needs the board on USB; ~4 ms/step over JTAG)
set -e
cd "$(dirname "$0")/.."
D=${DIFF_DIR:-hw}; FLASH_MB=${FLASH_MB:-16}; IMG=${FLASH_IMAGE:-$( [ -f $D/flash-8M.bin ] && echo $D/flash-8M.bin || echo $D/flash-0-1M.bin )}
N=${1:-3000}
OCD=$(ls -d ~/.espressif/tools/openocd-esp32/*/openocd-esp32 | head -1); SCR=$OCD/share/openocd/scripts
GDB=$(ls ~/.espressif/tools/xtensa-esp-elf-gdb/*/xtensa-esp-elf-gdb/bin/xtensa-esp32s3-elf-gdb | head -1)
pkill -f openocd-esp32 2>/dev/null || true; sleep 1
# 1) real efuses + strap
$OCD/bin/openocd -s $SCR -f board/esp32s3-builtin.cfg -c init -c halt -c "sleep 200" -c "mdw 0x60007000 128" -c "mdw 0x60004038" -c resume -c shutdown 2>&1 | grep -E "^0x6000" > $D/jtag-regs.txt
grep "^0x60007" $D/jtag-regs.txt > $D/efuse.txt
STRAP=$(grep "^0x60004038" $D/jtag-regs.txt | awk '{print $2}')
# 2) hardware step trace from reset
$OCD/bin/openocd -s $SCR -f board/esp32s3-builtin.cfg -c "esp32s3.cpu0 configure -rtos none" > $D/openocd.log 2>&1 & OPID=$!
sleep 4
$GDB -batch -nx -ex "set pagination off" -ex "set confirm off" -ex "target extended-remote :3333" -ex "monitor reset halt" -ex "flushregs" -ex "info registers pc" \
     -ex "set \$steps=$N" -ex "set \$outfile=\"$D/hw-trace.txt\"" -ex "source hw/steptrace.py" > $D/gdb.log 2>&1 || true
kill $OPID 2>/dev/null || true
# 3) emulator on the same image (bootloader + partition table + start of app dumped with esptool)
./target/release/esp32sim --boot rom --flash-mb $FLASH_MB --flash-image $IMG --efuse-regs $D/efuse.txt --regs-init $D/reset-regs.txt \
     --strap 0x$STRAP --regtrace $D/emu-trace.txt --regtrace-max $N --max-insns $N --console none --no-dump 2>/dev/null
# 4) compare
python3 hw/compare.py $D/hw-trace.txt $D/emu-trace.txt
