#!/bin/sh
# DEV-02 acceptance: esptool and idf.py flash an emulated NOTE over RFC 2217 through the ROM's
# download mode (auto-reset via DTR/RTS), the flash verifies, and the device boots the new image.
# Needs the imported ROM, .tools/fixtures/{zectrix-note4-epd-demo-v1.0.0.bin,diag-crypto}, and
# the old project's ESP-IDF tools (NOTE_IDF_TOOLS overrides).
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TOOLS=${NOTE_IDF_TOOLS:-${NOTE_PRIVATE_DIR:-$ROOT/.tools/private}/.device-tools}
ESPTOOL="$TOOLS/bin/esptool"
FW="$ROOT/.tools/fixtures/diag-crypto"
DEMO="$ROOT/third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"
PORT=${NOTE_SERIAL_PORT:-4010}
OUT=$(mktemp -d)
EMU=
trap '[ -n "$EMU" ] && kill "$EMU" 2>/dev/null; rm -rf "$OUT"' EXIT

[ -x "$ESPTOOL" ] || { echo "no esptool at $ESPTOOL" >&2; exit 1; }
[ -f "$FW/flasher_args.json" ] || { echo "run scripts/build-fixtures.sh diag-fw/crypto" >&2; exit 1; }
cargo build --release --quiet -p note-emu --manifest-path "$ROOT/Cargo.toml"

start() {
    "$ROOT/target/release/note-emu" --profile note4 --firmware "$DEMO" --serial-rfc2217 "$PORT" --realtime --seconds 0 \
        > "$OUT/$1.console" 2> "$OUT/$1.emu" &
    EMU=$!
    sleep 1
}
stop() { kill "$EMU" 2>/dev/null; wait "$EMU" 2>/dev/null || true; EMU=; }
fail=0
expect() {
    if grep -qaE "$3" "$OUT/$2"; then printf 'PASS  %s\n' "$1"; else printf 'FAIL  %s\n' "$1"; fail=1; fi
}

start esptool
timeout 120 "$ESPTOOL" --port "rfc2217://127.0.0.1:$PORT" --chip esp32s3 flash-id > "$OUT/flash-id.txt" 2>&1 || true
timeout 300 "$ESPTOOL" --port "rfc2217://127.0.0.1:$PORT" --chip esp32s3 write-flash \
    0x0 "$FW/bootloader/bootloader.bin" 0x8000 "$FW/partition_table/partition-table.bin" 0x10000 "$FW/diag_crypto.bin" \
    > "$OUT/write.txt" 2>&1 || true
sleep 3
stop
expect "auto-reset into download mode"      esptool.emu   "EN released, download boot"
expect "flash-id reads the 16 MB flash"      flash-id.txt  "Detected flash size: 16MB"
expect "stub flasher runs in emulated RAM"   write.txt     "Stub flasher running"
expect "every region verifies"               write.txt     "Hash of data verified"
expect "hard reset boots from flash"         esptool.emu   "EN released, SPI flash boot"
expect "device runs the flashed firmware"    esptool.console "DIAG done"

if [ -f "$TOOLS/esp-idf-v6.0/export.sh" ] && [ -d "$ROOT/.tools/fixtures-src/diag-crypto" ]; then
    start idf
    (export IDF_TOOLS_PATH="$TOOLS/idf-tools"; . "$TOOLS/esp-idf-v6.0/export.sh" > /dev/null 2>&1
     cd "$ROOT/.tools/fixtures-src/diag-crypto" && timeout 300 idf.py -B "$FW" -p "rfc2217://127.0.0.1:$PORT" flash) > "$OUT/idf.txt" 2>&1 || true
    sleep 3
    stop
    expect "idf.py flash completes"          idf.txt       "Hash of data verified"
    expect "idf.py-flashed firmware runs"    idf.console   "DIAG done"
fi
# idf.py monitor (esp-idf-monitor) over the same URL: resets the chip over DTR/RTS and streams
# the application's output. It needs a terminal, so it runs under script(1).
PYBIN=$(ls -d "$TOOLS"/idf-tools/python_env/*/bin 2>/dev/null | tail -1)
if [ -x "$PYBIN/python" ]; then
    start monitor
    # The emulator keeps the flash it booted with; flash the diag image again for a clean run.
    "$ESPTOOL" --port "rfc2217://127.0.0.1:$PORT" --chip esp32s3 write-flash \
        0x0 "$FW/bootloader/bootloader.bin" 0x8000 "$FW/partition_table/partition-table.bin" 0x10000 "$FW/diag_crypto.bin" \
        > /dev/null 2>&1 || true
    sleep 25 | timeout 20 script -q "$OUT/monitor.txt" "$PYBIN/python" -m esp_idf_monitor \
        --port "rfc2217://127.0.0.1:$PORT" --target esp32s3 "$FW/diag_crypto.elf" > /dev/null 2>&1 || true
    stop
    expect "idf.py monitor resets over RFC 2217"  monitor.emu   "EN released, SPI flash boot"
    expect "idf.py monitor streams app output"    monitor.txt   "DIAG sha256 "
fi
[ "$fail" = 0 ] || { for f in "$OUT"/*.txt "$OUT"/*.emu; do echo "--- $f"; tail -15 "$f"; done; exit 1; }
