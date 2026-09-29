#!/bin/sh
# pocket-tank: a page demo and the benchmark workload for language-model inference on PIE
# (docs/speed-plan.md). mediacutlet/pocket-tank (MIT) runs a 4-bit transformer on the Waveshare
# ESP32-S3-Touch-AMOLED-1.8 (board waveshare-amoled18-v2).
#   bootloader, partition table, app: committed under web/wasm/fw/public/ (pocket-tank-*.bin, with
#     pocket-tank-LICENSE.txt), the bytes of the project's browser installer, manifest version 8ea6436.
#     That host answers GitHub's runners with other bytes, so the Pages site serves its own copy.
#     This script checks them against their pinned SHA-256.
#   model: 7.56 MB, not committed; fetched from the repository at 5cc33b1 into web/wasm/fw/local/,
#     pinned by SHA-256. A file already present with the right hash is kept.
# The Pages workflow runs this too; it also writes the asset map for tools/browser-benchmark.
set -e
cd "$(dirname "$0")/.."
P=web/wasm/fw/public
D=web/wasm/fw/local
mkdir -p "$D"
MODEL=https://raw.githubusercontent.com/mediacutlet/pocket-tank/5cc33b1adf4076315b805e94129b295570a45b39/model/out/model_q4.bin
if command -v shasum >/dev/null 2>&1; then sum() { shasum -a 256 "$1" | cut -d' ' -f1; }
else sum() { sha256sum "$1" | cut -d' ' -f1; }; fi
check() {   # FILE SHA256
  got=$(sum "$1")
  if [ "$got" != "$2" ]; then echo "SHA-256 mismatch for $1: got $got" >&2; return 1; fi
  echo "ok $1"
}
check "$P/pocket-tank-bootloader.bin" 9556d59f3a10f6b04fa6d75f3142a66034339afafffbd4bbc3d9a127eaa73859 || exit 1
check "$P/pocket-tank-ptable.bin"     7af5b28608c91167e00778b7f20622f3b47849efebeb3e64117162d73f3e0508 || exit 1
check "$P/pocket-tank.bin"            443dd24aed14a509e6df597eb3a02635cc07e0fbee6f04db85a62f4d4dadb113 || exit 1
SHA=b200bf87c78dc85c4d727f68941ac0b83cced9827037cc169cd2122d71002c2a
if [ -f "$D/model_q4.bin" ] && [ "$(sum "$D/model_q4.bin")" = "$SHA" ]; then
  echo "ok (present) $D/model_q4.bin"
else
  curl -sfL --retry 5 --retry-all-errors --retry-delay 5 "$MODEL" -o "$D/model_q4.bin.part"
  mv "$D/model_q4.bin.part" "$D/model_q4.bin"
  check "$D/model_q4.bin" "$SHA" || { rm -f "$D/model_q4.bin"; exit 1; }
fi

# The asset map for tools/browser-benchmark (run-pairs.py --assets, serve.py): absolute paths,
# with the mask ROM from ESP32SIM_ROM_DIR, the page's copy, or the newest esp-rom-elfs install.
# Skipped on CI, where it would only publish the runner's paths.
[ -z "$CI" ] || exit 0
ROM=""
for c in "${ESP32SIM_ROM_DIR:+$ESP32SIM_ROM_DIR/esp32s3_rev0_rom.elf}" web/wasm/fw/esp32s3_rev0_rom.elf; do
  if [ -n "$c" ] && [ -f "$c" ]; then ROM="$c"; break; fi
done
[ -n "$ROM" ] || ROM=$(ls -d "$HOME"/.espressif/tools/esp-rom-elfs/*/esp32s3_rev0_rom.elf 2>/dev/null | sort | tail -1)
if [ -n "$ROM" ]; then
  ABS=$(pwd)
  case "$ROM" in /*) ;; *) ROM="$ABS/$ROM" ;; esac
  cat > "$D/pocket-tank-assets.json" <<JSON
{"workload": "pocket-tank", "rom": "$ROM", "bootloader": "$ABS/$P/pocket-tank-bootloader.bin", "ptable": "$ABS/$P/pocket-tank-ptable.bin", "app": "$ABS/$P/pocket-tank.bin", "model": "$ABS/$D/model_q4.bin"}
JSON
  echo "wrote $D/pocket-tank-assets.json for tools/browser-benchmark"
else
  echo "no esp32s3_rev0_rom.elf found: set ESP32SIM_ROM_DIR to also write the browser-benchmark asset map" >&2
fi
