#!/bin/sh
# DEV-01 acceptance: the ESP-IDF GDB debugs unmodified firmware through `note-emu --gdb`.
# Needs .tools/fixtures/diag-crypto (scripts/build-fixtures.sh diag-fw/crypto), the imported ROM,
# and esp-gdb (defaults to the old project's IDF tools; set NOTE_GDB_DIR to override).
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
GDB_DIR=${NOTE_GDB_DIR:-$(ls -d "${NOTE_PRIVATE_DIR:-$ROOT/.tools/private}"/.device-tools/idf-tools/tools/xtensa-esp-elf-gdb/*/xtensa-esp-elf-gdb 2>/dev/null | tail -1)}
GDB="$GDB_DIR/bin/xtensa-esp-elf-gdb-no-python"
# The no-Python GDB is generic Xtensa; the S3 core configuration selects the 212-register layout.
export XTENSA_GNU_CONFIG="$GDB_DIR/lib/xtensa_esp32s3.so"
FW="$ROOT/.tools/fixtures/diag-crypto"
PORT=${NOTE_GDB_PORT:-3333}
OUT=$(mktemp -d)
trap 'kill "$EMU" 2>/dev/null || true; rm -rf "$OUT"' EXIT

[ -x "$GDB" ] || { echo "no esp-gdb at $GDB (set NOTE_GDB_DIR)" >&2; exit 1; }
[ -f "$FW/flasher_args.json" ] || { echo "run scripts/build-fixtures.sh diag-fw/crypto" >&2; exit 1; }
cargo build --release --quiet -p note-emu --manifest-path "$ROOT/Cargo.toml"

"$ROOT/target/release/note-emu" --profile note4c --firmware "$FW" --gdb "$PORT" --gdb-wait --seconds 0 \
    > "$OUT/console.txt" 2> "$OUT/emu.txt" &
EMU=$!
sleep 1
timeout 300 "$GDB" -nx -batch "$FW/diag_crypto.elf" \
    -ex "set confirm off" -ex "target remote 127.0.0.1:$PORT" -ex "info threads" \
    -ex "break app_main" -ex "continue" -ex "bt 3" -ex "stepi" -ex "info registers pc" \
    -ex "break hash" -ex "continue" -ex "x/4xb buf" -ex "info threads" -ex "delete" -ex "detach" \
    > "$OUT/gdb.txt" 2>&1

fail=0
expect() {
    if grep -qE "$2" "$OUT/gdb.txt"; then printf 'PASS  %s\n' "$1"; else printf 'FAIL  %s\n' "$1"; fail=1; fi
}
expect "two cores as threads"            'Thread 2 \(core1 \(LX7\)\)'
expect "break at app_main on core 0"     'Thread 1 hit Breakpoint 1, app_main \(\)'
expect "backtrace through FreeRTOS"      'in vPortTaskWrapper'
expect "stepi advances the pc"           'pc +0x[0-9a-f]+ +0x[0-9a-f]+ <app_main\+[1-9]'
expect "read a known global"             '<buf>:[[:space:]]+0x03[[:space:]]+0x0a[[:space:]]+0x11[[:space:]]+0x18'
expect "second core idles in FreeRTOS"   'Thread 2 \(core1 \(LX7\)\) .*esp_cpu_wait_for_intr'
expect "detach"                          'detached'
[ "$fail" = 0 ] || { sed 's/^/      /' "$OUT/gdb.txt" | head -60; exit 1; }
