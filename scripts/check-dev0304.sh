#!/bin/sh
# DEV-03 (panic symbolication) and DEV-04 (core dump) acceptance on fixtures/diag-fw/panic.
# Needs .tools/fixtures/diag-panic (scripts/build-fixtures.sh diag-fw/panic), the imported ROM,
# esp-gdb and ESP-IDF's esp-coredump (default: the old project's IDF tools; NOTE_GDB_DIR and
# NOTE_IDF_PYTHON_BIN override).
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
GDB_DIR=${NOTE_GDB_DIR:-$(ls -d "${NOTE_PRIVATE_DIR:-$ROOT/.tools/private}"/.device-tools/idf-tools/tools/xtensa-esp-elf-gdb/*/xtensa-esp-elf-gdb 2>/dev/null | tail -1)}
PYBIN=${NOTE_IDF_PYTHON_BIN:-$(ls -d "${NOTE_PRIVATE_DIR:-$ROOT/.tools/private}"/.device-tools/idf-tools/python_env/*/bin 2>/dev/null | tail -1)}
GDB="$GDB_DIR/bin/xtensa-esp-elf-gdb-no-python"
export XTENSA_GNU_CONFIG="$GDB_DIR/lib/xtensa_esp32s3.so"
FW="$ROOT/.tools/fixtures/diag-panic"
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

[ -x "$GDB" ] || { echo "no esp-gdb at $GDB" >&2; exit 1; }
[ -x "$PYBIN/esp-coredump" ] || { echo "no esp-coredump in $PYBIN" >&2; exit 1; }
[ -f "$FW/flasher_args.json" ] || { echo "run scripts/build-fixtures.sh diag-fw/panic" >&2; exit 1; }
cargo build --release --quiet -p note-emu --manifest-path "$ROOT/Cargo.toml"

"$ROOT/target/release/note-emu" --profile note4c --firmware "$FW" --seconds 2 \
    --coredump-out "$OUT/core.bin" > "$OUT/console.txt" 2> "$OUT/emu.txt"
"$PYBIN/esp-coredump" --chip esp32s3 info_corefile --gdb "$GDB" --core "$OUT/core.bin" --core-format raw \
    --save-core "$OUT/core.elf" "$FW/diag_panic.elf" > "$OUT/coreinfo.txt" 2>&1
# The same dump through a stored AVD: `ndb -s ID coredump` reads the committed flash.
cargo build --release --quiet -p ndb --manifest-path "$ROOT/Cargo.toml"
AVD_HOME="$OUT/home"; mkdir -p "$AVD_HOME"
ROM_ELF=$(env -u NOTE_EMU_HOME "$ROOT/target/release/ndb" --json rom status | python3 -c 'import json,sys; print(json.load(sys.stdin)["rom"]["path"])')
NOTE_EMU_HOME="$AVD_HOME" "$ROOT/target/release/ndb" rom import "$ROM_ELF" > /dev/null
ID=$(NOTE_EMU_HOME="$AVD_HOME" "$ROOT/target/release/ndb" avd create --profile note4c --firmware "$FW")
NOTE_EMU_HOME="$AVD_HOME" "$ROOT/target/release/note-emu" --avd "$ID" --seconds 2 > /dev/null 2>&1
NOTE_EMU_HOME="$AVD_HOME" "$ROOT/target/release/ndb" -s "$ID" coredump -o "$OUT/avd-core.bin" > /dev/null 2>&1 || true
"$PYBIN/esp-coredump" --chip esp32s3 info_corefile --gdb "$GDB" --core "$OUT/avd-core.bin" --core-format raw \
    "$FW/diag_panic.elf" > "$OUT/avd-coreinfo.txt" 2>&1 || true
"$GDB" -nx -batch "$FW/diag_panic.elf" "$OUT/core.elf" -ex "bt" -ex "print/x local_marker" > "$OUT/core-gdb.txt" 2>&1

line=$(grep -n '/\* FAULT \*/' "$ROOT/fixtures/diag-fw/panic/main/main.c" | cut -d: -f1)
fail=0
expect() {
    if grep -qE "$3" "$OUT/$2"; then printf 'PASS  %s\n' "$1"; else printf 'FAIL  %s\n' "$1"; fail=1; fi
}
expect "DEV-03 fault frame is diag_panic_here:$line" console.txt "#0 0x[0-9a-f]+ diag_panic_here at main/main.c:$line\$"
expect "DEV-03 caller frame symbolized"             console.txt "#1 0x[0-9a-f]+ diag_crash_task at main/main.c:"
expect "DEV-04 core dump extracted from flash"      emu.txt     "core dump partition written"
expect "DEV-04 crashing task name"                  coreinfo.txt "Crashed task handle: .*name: 'diag_crash'"
expect "DEV-04 exception cause"                     coreinfo.txt "StoreProhibitedCause"
expect "DEV-04 crashing PC/function"                coreinfo.txt "<diag_panic_here\+"
expect "DEV-04 frame 0 from the core"               core-gdb.txt "#0 .* in diag_panic_here .*main.c:$line"
expect "DEV-04 ndb coredump from a stored AVD"   avd-coreinfo.txt "Crashed task handle: .*name: 'diag_crash'"
expect "DEV-04 known local variable"                core-gdb.txt '^\$1 = 0x5a5a$'
[ "$fail" = 0 ] || { for f in console.txt coreinfo.txt core-gdb.txt; do echo "--- $f"; tail -30 "$OUT/$f"; done; exit 1; }
