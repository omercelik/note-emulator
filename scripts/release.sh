#!/bin/sh
# Build, sign with a Developer ID, notarize and staple NOTE Emulator.app, then package a DMG for a
# GitHub release.
#
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/release.sh
#
# Notarization uses a notarytool keychain profile (default "note-emulator"), created once with
#   xcrun notarytool store-credentials note-emulator --apple-id <email> --team-id <TEAMID>
# Output: dist/release/NOTE-Emulator-<version>-macos-arm64.dmg and its .sha256.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
: "${SIGN_IDENTITY:?set SIGN_IDENTITY to a Developer ID Application identity}"
PROFILE=${NOTARY_PROFILE:-note-emulator}
APP="$ROOT/dist/NOTE Emulator.app"
OUT="$ROOT/dist/release"

SIGN_IDENTITY="$SIGN_IDENTITY" "$ROOT/scripts/bundle-app.sh"
codesign --verify --strict --deep "$APP"
codesign -dv "$APP" 2>&1 | grep -E "^Authority=Developer ID Application|^TeamIdentifier" 

mkdir -p "$OUT"
# ditto keeps the bundle's signature and extended attributes intact (zip -r does not).
ditto -c -k --keepParent "$APP" "$OUT/submit.zip"
xcrun notarytool submit "$OUT/submit.zip" --keychain-profile "$PROFILE" --wait
rm -f "$OUT/submit.zip"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
spctl --assess --type execute --verbose=2 "$APP"

SIGN_IDENTITY="$SIGN_IDENTITY" NOTARY_PROFILE="$PROFILE" "$ROOT/scripts/package-dmg.sh"
ls -l "$OUT"
