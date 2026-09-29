#!/bin/sh
# Gate report (proposal §12). Usage: scripts/verify.sh [--engine] [--firmware] [--dev]
#   --engine    also run the vendored esp32sim suite incl. goldens (slow, needs ROM ELFs)
#   --firmware  also run unmodified-firmware regression tests (needs imported ROM + fixtures)
#   --dev       also run developer-workflow acceptance (DEV-01 GDB, DEV-02 flashing, DEV-03/04 panic + core;
#               needs esp-gdb and esp-coredump)
#   --package   also build dist/NOTE Emulator.app and run the sandboxed PKG-01 stand-in
set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ROM_ELFS=${ESP32SIM_ROM_DIR:-${NOTE_PRIVATE_DIR:-$ROOT/.tools/private}/.device-tools/idf-tools/tools/esp-rom-elfs/20241011}
ENGINE=0; FIRMWARE=0; DEV=0; PACKAGE=0
for arg in "$@"; do
    [ "$arg" = "--engine" ] && ENGINE=1
    [ "$arg" = "--firmware" ] && FIRMWARE=1
    [ "$arg" = "--dev" ] && DEV=1
    [ "$arg" = "--package" ] && PACKAGE=1
done

pass=0; fail=0
check() {
    name=$1; shift
    if out=$("$@" 2>&1); then
        printf 'PASS  %s\n' "$name"; pass=$((pass + 1))
    else
        printf 'FAIL  %s\n' "$name"; printf '%s\n' "$out" | tail -15 | sed 's/^/      /'; fail=$((fail + 1))
    fi
}

cd "$ROOT" || exit 1
check "rust workspace builds"          cargo build --workspace --quiet
check "rust unit tests"                cargo test --workspace --quiet
check "swift package builds"           swift build --package-path macos --quiet
check "swift tests"                    swift test --package-path macos --quiet
check "protocol fixtures identical"    cmp crates/note-protocol/fixtures/hello-request.bin macos/Tests/NoteProtocolTests/Fixtures/hello-request.bin
check "engine builds"                  cargo build --release --quiet --manifest-path engine/esp32sim/Cargo.toml -p esp32sim --bin esp32sim
check "ndb profiles"                   cargo run --quiet -p ndb -- profiles
if [ "$ENGINE" = 1 ]; then
    check "engine suite + goldens"     env ESP32SIM_ROM_DIR="$ROM_ELFS" cargo test --release --quiet \
                                           --manifest-path engine/esp32sim/Cargo.toml --workspace -- --include-ignored --skip external_
fi
check "ROM imported (ndb doctor)"      cargo run --quiet -p ndb -- doctor
if [ "$DEV" = 1 ]; then
    check "DEV-01 gdb acceptance"      scripts/check-dev01.sh
    check "DEV-02 esptool/idf.py flash" scripts/check-dev02.sh
    check "DEV-03/04 panic and core"   scripts/check-dev0304.sh
fi
if [ "$FIRMWARE" = 1 ]; then
    check "unmodified firmware regressions" cargo test --release --quiet -p note-machine --test firmware -- --ignored
fi
if [ "$PACKAGE" = 1 ]; then
    check "G8 bundle + audit"          scripts/bundle-app.sh
    check "PKG-01 stand-in (sandboxed)" scripts/check-pkg.sh
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
