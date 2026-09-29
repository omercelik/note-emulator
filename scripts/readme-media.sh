#!/bin/sh
# Regenerate the README images in docs/media from live devices, rendered with the app's own
# views (macos/Tests/NoteEmulatorTests/ReadmeMediaTests.swift). Uses a scratch data home, so
# your own devices are untouched.
#
#   scripts/readme-media.sh
#
# The NOTE4 part needs only the demo fixture (.tools/fixtures). The NOTE4C part is optional and
# talks to Gemini Live: set GEMINILIVE_IMAGE (a merged 16 MB image of note4c-geminilive) and
# GEMINILIVE_ENV (its mode-0600 .env, for the Wi-Fi name and password). It never records the
# Home screen, which shows the city compiled into that build.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUT="$ROOT/docs/media"
ROM=${ROM:-"$HOME/Library/Application Support/NOTE Emulator/rom/esp32s3_rev0_rom.elf"}
DEMO="$ROOT/third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"
NDB="$ROOT/target/release/ndb"
EMU="$ROOT/target/release/note-emu"
mkdir -p "$OUT"

cargo build --release -q -p note-emu -p ndb --manifest-path "$ROOT/Cargo.toml"
swift build --package-path "$ROOT/macos" --build-tests -q

HOME_DIR=$(mktemp -d)
export NOTE_EMU_HOME="$HOME_DIR" NOTE_EMU_BIN="$EMU" NDB_BIN="$NDB"
PIDS=""
cleanup() {
    for pid in $PIDS; do kill -TERM "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
    rm -rf "$HOME_DIR" "${WORK:-}"
}
trap cleanup EXIT
WORK=$(mktemp -d)
"$NDB" rom import "$ROM" >/dev/null

# media MODE OUTFILE [VAR=value ...]: one render through the gated Swift test.
media() {
    mode=$1; file=$2; shift 2
    env NOTE_MEDIA_MODE="$mode" NOTE_MEDIA_OUT="$OUT/$file" "$@" \
        swift test --package-path "$ROOT/macos" --skip-build --filter readmeMedia 2>&1 \
        | grep -E "readme-media:|recorded an issue|error" || true
}
# boot ID: run the AVD headless in real time; prints its control socket once it answers.
boot() {
    "$EMU" --avd "$1" --seconds 0 --realtime --entropy host >"$WORK/$1.log" 2>&1 &
    PIDS="$PIDS $!"
    i=0
    until "$NDB" -s "$1" status >/dev/null 2>&1; do
        i=$((i + 1)); [ $i -lt 300 ] || { echo "$1 did not start" >&2; exit 1; }
        sleep 0.1
    done
    "$NDB" -s "$1" --json status >/dev/null
    python3 -c "import json,glob,sys
for f in glob.glob(sys.argv[1] + '/run/*/instance.json'):
    d = json.load(open(f))
    if d['avd'] == sys.argv[2]: print(d['sock'])" "$HOME_DIR" "$1"
}
# talk ID WAV: hold OK while the question plays into the microphone.
talk() {
    hold=$(afinfo "$2" | awk '/estimated duration/ { printf "%d", ($3 + 0.8) * 1000 }')
    "$NDB" -s "$1" press ok --hold "$hold" >/dev/null &
    sleep 0.4
    "$NDB" -s "$1" mic "$2" >/dev/null
    wait $!
}

# --- NOTE4: from power-on through the menu into the 16-gray gallery -------------------------
N4=$("$NDB" avd create --profile note4 --firmware "$DEMO" --name "NOTE4 demo" | tail -1)
SOCK4=$(boot "$N4")
( media anim note4-gallery.png NOTE_MEDIA_SOCK="$SOCK4" NOTE_MEDIA_SECONDS=17 ) &
REC=$!
# The same timeline as note4_demo_draws_the_16_gray_gallery_scene (seconds after power-on).
sleep 4;   "$NDB" -s "$N4" press down >/dev/null   # DISPLAY GALLERY
sleep 1.5; "$NDB" -s "$N4" press ok >/dev/null
sleep 2;   "$NDB" -s "$N4" press down >/dev/null   # PARTIAL
sleep 1;   "$NDB" -s "$N4" press down >/dev/null   # 16-GRAY / 4BPP
sleep 1.5; "$NDB" -s "$N4" press ok >/dev/null
wait $REC

# --- NOTE4C: Gemini Live pins a card while the status LED is lit ----------------------------
# screen ID: a hash of the panel right now, to tell Home from a card without looking at it.
screen() { "$NDB" -s "$1" screenshot -o "$WORK/screen.png" >/dev/null && shasum "$WORK/screen.png" | cut -c1-40; }
# changed_from ID HASH SECONDS: wait until the panel differs from HASH.
changed_from() {
    i=0
    while [ $i -lt "$3" ]; do
        [ "$(screen "$1")" != "$2" ] && return 0
        sleep 1; i=$((i + 1))
    done
    return 1
}
gemini_log() { grep -E "session:|tools:|ws:|controls:" "$WORK/$1.log" | tail -12 >&2 || true; }
SOCK4C=""
if [ -n "${GEMINILIVE_IMAGE:-}" ] && [ -n "${GEMINILIVE_ENV:-}" ]; then
    GL=$("$NDB" avd create --profile note4c --firmware "$GEMINILIVE_IMAGE" --name "NOTE4C Gemini Live" | tail -1)
    "$NDB" avd wifi "$GL" --env "$GEMINILIVE_ENV" >/dev/null
    "$NDB" avd network "$GL" user >/dev/null
    SOCK=$(boot "$GL")
    # Home appears once Wi-Fi, time and weather are in; it shows the build's city, so it is
    # never recorded. Wait for it, remember it, and put a card over it first.
    BLANK=$(screen "$GL")
    changed_from "$GL" "$BLANK" 90 || { echo "NOTE4C: no Home screen" >&2; gemini_log "$GL"; }
    sleep 3
    HOME_SCREEN=$(screen "$GL")
    say -o "$WORK/q1.wav" --file-format=WAVE --data-format=LEI16@16000 --channels=1 \
        "Pin three short facts about the Moon to the screen."
    say -o "$WORK/q2.wav" --file-format=WAVE --data-format=LEI16@16000 --channels=1 \
        "Pin a list of the four inner planets, closest to the Sun first."
    covered=""
    for attempt in 1 2 3; do
        talk "$GL" "$WORK/q1.wav"
        if changed_from "$GL" "$HOME_SCREEN" 45; then covered=yes; break; fi
        echo "NOTE4C: no card after attempt $attempt" >&2
    done
    if [ -n "$covered" ]; then
        sleep 3
        BEFORE=$(screen "$GL")
        ( media anim note4c-gemini.png NOTE_MEDIA_SOCK="$SOCK" NOTE_MEDIA_SECONDS=30 ) &
        REC=$!
        sleep 3
        talk "$GL" "$WORK/q2.wav"
        wait $REC
        if [ "$(screen "$GL")" = "$BEFORE" ]; then
            echo "NOTE4C: the second card did not arrive; animation discarded" >&2
            rm -f "$OUT/note4c-gemini.png"; gemini_log "$GL"
        fi
        [ "$(screen "$GL")" != "$HOME_SCREEN" ] && SOCK4C=$SOCK
    else
        echo "NOTE4C: skipped (Gemini never put a card up; nothing recorded)" >&2
        gemini_log "$GL"
    fi
fi

# --- Stills ---------------------------------------------------------------------------------
if [ -n "$SOCK4C" ]; then
    media pair devices.png NOTE_MEDIA_SOCK="$SOCK4" NOTE_MEDIA_SOCK2="$SOCK4C"
else
    media still devices.png NOTE_MEDIA_SOCK="$SOCK4"
fi
media controls controls.png NOTE_MEDIA_SOCK="$SOCK4"
"$NDB" -s "$N4" stop >/dev/null
[ -n "${GL:-}" ] && "$NDB" -s "$GL" stop >/dev/null
media manager manager.png NOTE_MEDIA_START="$N4"
ls -l "$OUT"
