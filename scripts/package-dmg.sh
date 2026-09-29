#!/bin/sh
# Package an existing signed app as a signed, notarized drag-to-install DMG.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
: "${SIGN_IDENTITY:?set SIGN_IDENTITY to a Developer ID Application identity}"
PROFILE=${NOTARY_PROFILE:-note-emulator}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
APP="$ROOT/dist/NOTE Emulator.app"
OUT="$ROOT/dist/release"
DMG="$OUT/NOTE-Emulator-$VERSION-macos-arm64.dmg"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

codesign --verify --strict --deep "$APP"
mkdir -p "$OUT"
ditto "$APP" "$STAGE/NOTE Emulator.app"
ln -s /Applications "$STAGE/Applications"
hdiutil create -ov -volname "NOTE Emulator $VERSION" -srcfolder "$STAGE" -format UDZO "$DMG"
codesign --force --timestamp -s "$SIGN_IDENTITY" "$DMG"
codesign --verify --strict "$DMG"
xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
( cd "$OUT" && shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256" )
