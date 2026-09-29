#!/bin/sh
# Build dist/NOTE Emulator.app: the SwiftUI app with note-emu, ndb, note-net-helper, profiles,
# skins, the ESP32-S3 mask ROM and the NOTE4 demo firmware (ADR-018). Ad-hoc signed, hardened
# runtime; note-emu gets com.apple.security.cs.allow-jit (the AArch64 JIT maps MAP_JIT pages).
# Ends with an audit: only system libraries and no developer paths in any bundled binary.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DIST=${NOTE_EMU_DIST:-"$ROOT/dist"}
APP="$DIST/NOTE Emulator.app"
TARGET="$ROOT/target/bundle"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)

echo "building Rust tools (paths remapped)"
RUSTFLAGS="--remap-path-prefix=$ROOT=. --remap-path-prefix=$HOME/.cargo=cargo --remap-path-prefix=$HOME/.rustup=rustup" \
    CARGO_TARGET_DIR="$TARGET" cargo build --release --quiet --manifest-path "$ROOT/Cargo.toml" \
    -p note-emu -p ndb -p note-net-helper
echo "building the app"
mkdir -p "$DIST"
swift build -c release --package-path "$ROOT/macos" \
    -Xswiftc -file-prefix-map -Xswiftc "$ROOT=." -Xswiftc -file-prefix-map -Xswiftc "$HOME=~" \
    > "$DIST/swift-build.log" 2>&1 || { cat "$DIST/swift-build.log"; exit 1; }
SWIFT_BIN=$(swift build -c release --package-path "$ROOT/macos" --show-bin-path)

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$SWIFT_BIN/NoteEmulator" "$APP/Contents/MacOS/NoteEmulator"
for tool in note-emu ndb note-net-helper; do
    cp "$TARGET/release/$tool" "$APP/Contents/MacOS/$tool"
done
mkdir -p "$APP/Contents/Resources/Profiles/skins"
cp "$ROOT"/Profiles/*.json "$APP/Contents/Resources/Profiles/"
cp "$ROOT"/Profiles/skins/* "$APP/Contents/Resources/Profiles/skins/"
cp -R "$ROOT/licenses" "$APP/Contents/Resources/licenses" 2>/dev/null || true
# The ESP32-S3 mask ROM (Apache-2.0) and the NOTE4 demo firmware (MIT) ship with the app
# (ADR-018): the ROM is imported on first run, the demo is offered when there is no device yet.
mkdir -p "$APP/Contents/Resources/rom" "$APP/Contents/Resources/samples"
cp "$ROOT/third_party/esp-rom-elfs/esp32s3_rev0_rom.elf" "$APP/Contents/Resources/rom/"
cp "$ROOT/third_party/esp-rom-elfs/LICENSE" "$APP/Contents/Resources/rom/LICENSE"
cp "$ROOT/third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin" "$APP/Contents/Resources/samples/"
cp "$ROOT/third_party/zectrix-note4-epd-demo/LICENSE" "$APP/Contents/Resources/samples/LICENSE"
# The supplied NOTE4C factory image is local-only. Extract firmware, never saved device data.
FACTORY=${NOTE4C_FACTORY_IMAGE:-${NOTE_PRIVATE_DIR:-$ROOT/.tools/private}/.device-backup/factory-2026-09-23.bin}
if [ -f "$FACTORY" ]; then
    python3 "$ROOT/scripts/prepare-note4c-sample.py" "$FACTORY" "$APP/Contents/Resources/samples/note4c-factory.bin"
fi
cp "$ROOT/scripts/install-helper.sh" "$APP/Contents/Resources/install-helper.sh"
chmod 755 "$APP/Contents/Resources/install-helper.sh"

# App icon: macos/Resources/AppIcon.svg (original artwork) at every size macOS asks for.
ICONSET="$DIST/AppIcon.iconset"
rm -rf "$ICONSET"; mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    swift "$ROOT/scripts/svg2png.swift" "$ROOT/macos/Resources/AppIcon.svg" "$ICONSET/icon_${size}x${size}.png" "$size" 2>/dev/null
    swift "$ROOT/scripts/svg2png.swift" "$ROOT/macos/Resources/AppIcon.svg" "$ICONSET/icon_${size}x${size}@2x.png" "$((size * 2))" 2>/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>NOTE Emulator</string>
    <key>CFBundleDisplayName</key><string>NOTE Emulator</string>
    <key>CFBundleIdentifier</key><string>dev.note-emulator.app</string>
    <key>CFBundleExecutable</key><string>NoteEmulator</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>15.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSMicrophoneUsageDescription</key><string>Lets an emulated device hear this Mac's microphone when you turn it on in its Audio controls.</string>
</dict>
</plist>
PLIST

# Debug info carries build paths; the bundle ships without it.
for bin in "$APP"/Contents/MacOS/*; do strip -S "$bin" 2>/dev/null || true; done

ENT="$DIST/note-emu.entitlements"
cat > "$ENT" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.cs.allow-jit</key><true/>
</dict>
</plist>
PLIST
# SIGN_IDENTITY: "-" (ad-hoc, the default, for local use) or a Developer ID Application identity
# for a release (scripts/release.sh); a real identity also gets Apple's secure timestamp, which
# notarization requires.
SIGN_IDENTITY=${SIGN_IDENTITY:--}
if [ "$SIGN_IDENTITY" = "-" ]; then STAMP=--timestamp=none; else STAMP=--timestamp; fi
sign() { codesign --force --options runtime "$STAMP" -s "$SIGN_IDENTITY" "$@"; }
sign --entitlements "$ENT" "$APP/Contents/MacOS/note-emu"
for tool in ndb note-net-helper; do
    sign "$APP/Contents/MacOS/$tool"
done
# The app itself records from the microphone only when a device's Audio controls ask it to.
APP_ENT="$DIST/app.entitlements"
cat > "$APP_ENT" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.device.audio-input</key><true/>
</dict>
</plist>
PLIST
sign --entitlements "$APP_ENT" "$APP"
codesign --verify --strict --deep "$APP"

echo "audit"
fail=0
for bin in "$APP"/Contents/MacOS/*; do
    libs=$(otool -L "$bin" | tail -n +2 | awk '{print $1}' | grep -vE '^(/usr/lib/|/System/Library/)' || true)
    if [ -n "$libs" ]; then echo "FAIL  $(basename "$bin") links $libs"; fail=1; fi
    leaks=$(strings -a "$bin" | grep -E "$HOME|/opt/homebrew|/usr/local/Cellar|\.device-tools|\.tools/private" | head -3 || true)
    if [ -n "$leaks" ]; then echo "FAIL  $(basename "$bin") contains developer paths:"; echo "$leaks" | sed 's/^/        /'; fail=1; fi
done
[ "$fail" = 0 ] && echo "PASS  system libraries only, no developer paths"
du -sh "$APP" | sed 's/^/size  /'
exit "$fail"
