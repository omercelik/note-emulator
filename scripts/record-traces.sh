#!/bin/sh
# Record golden console/frame traces from the OLD QEMU emulator (read-only use of
# $NOTE_PRIVATE_DIR, default .tools/private) for the replay backend (proposal §10). Raw captures can contain
# device identifiers or credentials, so they go to .tools/traces/ (gitignored); only the
# manifest with hashes and milestone counts is committed.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OLD=${NOTE4C_OLD:-${NOTE_PRIVATE_DIR:-$ROOT/.tools/private}}
OUT="$ROOT/.tools/traces"
MANIFEST="$ROOT/fixtures/traces/manifest.tsv"
SECONDS_PER_RUN=${TRACE_SECONDS:-45}
mkdir -p "$OUT" "$(dirname "$MANIFEST")"

[ -f "$OLD/note4c_emulator.py" ] || { echo "old emulator not found at $OLD" >&2; exit 1; }

record() {
    id=$1; image=$2
    log="$OUT/$id.log"
    printf 'recording %s (%ss)...\n' "$id" "$SECONDS_PER_RUN" >&2
    # The old runner uses a temporary flash copy and QEMU snapshot mode; the source is untouched.
    (cd "$OLD" && python3 note4c_emulator.py "$image" --note4c --timeout "$SECONDS_PER_RUN" --log "$log") || true
    frames=$(grep -c '^BEGIN_FRAME' "$log" || true)
    lines=$(wc -l < "$log" | tr -d ' ')
    sha=$(shasum -a 256 "$log" | cut -d' ' -f1)
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$id" "$image" "$SECONDS_PER_RUN" "$lines" "$frames" "$sha" >> "$MANIFEST.new"
}

printf 'id\tsource\tseconds\tlines\tframes\tsha256\n' > "$MANIFEST.new"
record note4c-factory      .device-backup/factory-2026-09-23.bin
record emini-home-legacy   .device-tools/emini-home/firmware/build-emulator
record friday-legacy       .device-tools/friday-source/build-emulator
mv "$MANIFEST.new" "$MANIFEST"
cat "$MANIFEST"
