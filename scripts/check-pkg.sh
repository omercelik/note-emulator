#!/bin/sh
# PKG-01 stand-in (G8): the bundle from scripts/bundle-app.sh runs with the developer machine's
# toolchains and the old project unreadable (sandbox-exec). Imports the ROM only through the
# bundled ndb, creates and runs a NOTE4 AVD with the JIT and the interpreter, and starts the
# app. The real PKG-01 (fresh macOS account or clean VM) still needs a person.
# Needs the imported ROM (source for the copy) and third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
APP=${NOTE_EMU_APP:-"$ROOT/dist/NOTE Emulator.app"}
[ -d "$APP" ] || { echo "run scripts/bundle-app.sh first" >&2; exit 1; }
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$ROOT/third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin" "$WORK/demo.bin"
cp -R "$APP" "$WORK/"
M="$WORK/NOTE Emulator.app/Contents/MacOS"
[ -x "$WORK/NOTE Emulator.app/Contents/Resources/install-helper.sh" ] || { echo "FAIL  bundled helper installer missing"; exit 1; }
cat > "$WORK/isolate.sb" <<SB
(version 1)
(allow default)
(deny file-read* (subpath "$ROOT") (subpath "$HOME/Documents") (subpath "/opt/homebrew") (subpath "/usr/local")
    (subpath "$HOME/.cargo") (subpath "$HOME/.rustup") (subpath "$HOME/.espressif")
    (subpath "/Applications/Xcode.app") (subpath "/Library/Developer"))
SB
export NOTE_EMU_HOME="$WORK/home"
cd "$WORK"
fail=0
step() {
    name=$1; shift
    if sandbox-exec -f "$WORK/isolate.sb" "$@" > "$WORK/step.log" 2>&1; then echo "PASS  $name"
    else echo "FAIL  $name"; tail -5 "$WORK/step.log" | sed 's/^/      /'; fail=1; fi
}
step "developer folders unreadable inside the sandbox" sh -c "! ls '$ROOT' >/dev/null 2>&1"
[ -f "$WORK/NOTE Emulator.app/Contents/Resources/rom/esp32s3_rev0_rom.elf" ] || { echo "FAIL  bundled ROM missing"; exit 1; }
[ -f "$WORK/NOTE Emulator.app/Contents/Resources/samples/zectrix-note4-epd-demo-v1.0.0.bin" ] || { echo "FAIL  bundled NOTE4 demo missing"; exit 1; }
step "first-run import of the bundled ROM"      "$M/ndb" rom import
step "doctor passes (bundled profiles, ROM)"    "$M/ndb" doctor
ID=$(sandbox-exec -f "$WORK/isolate.sb" "$M/ndb" avd create --profile note4 --firmware demo.bin)
step "AVD runs with the JIT"                    "$M/note-emu" --avd "$ID" --seconds 3
grep -q "2 frames" "$WORK/step.log" || { echo "FAIL  JIT run drew no frames"; fail=1; }
step "AVD runs on the interpreter"              "$M/note-emu" --avd "$ID" --seconds 3 --no-jit
EMINI="$WORK/NOTE Emulator.app/Contents/Resources/samples/emini-home-0.6.2-note4c-merged.bin"
[ -f "$EMINI" ] || { echo "FAIL  bundled NOTE4C emini Home missing"; exit 1; }
[ ! -f "$WORK/NOTE Emulator.app/Contents/Resources/samples/note4c-factory.bin" ] || { echo "FAIL  private factory image bundled"; exit 1; }
EMINI_ID=$(sandbox-exec -f "$WORK/isolate.sb" "$M/ndb" avd create --profile note4c --firmware "$EMINI")
step "bundled NOTE4C emini Home boots" "$M/note-emu" --avd "$EMINI_ID" --seconds 4
grep -qE ', [1-9][0-9]* frames' "$WORK/step.log" || { echo "FAIL  NOTE4C demo drew no frame"; fail=1; }
sandbox-exec -f "$WORK/isolate.sb" "$M/NoteEmulator" > "$WORK/gui.log" 2>&1 &
GUI=$!
sleep 4
if kill -0 "$GUI" 2>/dev/null; then echo "PASS  app starts and stays up"; kill "$GUI"; else echo "FAIL  app exited"; tail -5 "$WORK/gui.log"; fail=1; fi
exit "$fail"
