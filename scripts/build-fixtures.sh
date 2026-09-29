#!/bin/sh
# Build firmware fixtures that are reproducible from public sources (Spec §17): stock ESP-IDF
# examples and the diagnostic programs in fixtures/diag-fw. Uses ESP-IDF 6.0 from the old project's
# tool directory at build time only (proposal §12); nothing from it is needed at runtime.
# Outputs go to .tools/fixtures/<name>/ (gitignored).
#
# Usage: scripts/build-fixtures.sh [wifi-station] [http-server] [https-request] [diag-fw/<name> ...]   (default: everything)
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TOOLS=${NOTE_IDF_TOOLS:-${NOTE_PRIVATE_DIR:-$ROOT/.tools/private}/.device-tools}
IDF_PATH=${IDF_PATH:-$TOOLS/esp-idf-v6.0}
export IDF_TOOLS_PATH=${IDF_TOOLS_PATH:-$TOOLS/idf-tools}
SRC="$ROOT/.tools/fixtures-src"
OUT="$ROOT/.tools/fixtures"

[ -f "$IDF_PATH/export.sh" ] || { echo "ESP-IDF not found at $IDF_PATH (set IDF_PATH)" >&2; exit 1; }
# shellcheck disable=SC1091
. "$IDF_PATH/export.sh" > /dev/null

build() {
    name=$1; dir=$2
    printf 'building %s\n' "$name" >&2
    (cd "$dir" && idf.py -B "$OUT/$name" set-target esp32s3 > "$OUT/$name.log" 2>&1 && idf.py -B "$OUT/$name" build >> "$OUT/$name.log" 2>&1) \
        || { echo "build of $name failed; see $OUT/$name.log" >&2; exit 1; }
    shasum -a 256 "$OUT/$name"/*.bin | sed "s|$OUT/||"
}

# example <fixture-name> <path under $IDF_PATH/examples> <sdkconfig lines...>
example() {
    name=$1; path=$2; shift 2
    rm -rf "$SRC/$name"
    mkdir -p "$SRC" "$OUT"
    cp -R "$IDF_PATH/examples/$path" "$SRC/$name"
    rm -rf "$SRC/$name/build" "$SRC/$name/sdkconfig"
    # Examples that share protocol_examples_common find it relative to IDF.
    if grep -q "protocol_examples_common" "$SRC/$name/CMakeLists.txt" "$SRC/$name/main/idf_component.yml" 2>/dev/null; then
        sed -i '' "s|../../common_components|$IDF_PATH/examples/common_components|g; s|../../../common_components|$IDF_PATH/examples/common_components|g" \
            "$SRC/$name/main/idf_component.yml" 2>/dev/null || true
    fi
    for line in "$@"; do printf '%s\n' "$line" >> "$SRC/$name/sdkconfig.defaults"; done
    build "$name" "$SRC/$name"
}

# Both join the emulator's open virtual AP (esp32sim `--wifi` default).
wifi_station() {
    example wifi-station wifi/getting_started/station \
        'CONFIG_ESP_WIFI_SSID="esp32sim"' 'CONFIG_ESP_WIFI_PASSWORD=""' 'CONFIG_ESP_WIFI_AUTH_OPEN=y'
}

http_server() {
    example http-server protocols/http_server/simple \
        'CONFIG_EXAMPLE_CONNECT_WIFI=y' 'CONFIG_EXAMPLE_CONNECT_ETHERNET=n' \
        'CONFIG_EXAMPLE_WIFI_SSID="esp32sim"' 'CONFIG_EXAMPLE_WIFI_PASSWORD=""' \
        'CONFIG_EXAMPLE_WIFI_AUTH_OPEN=y' 'CONFIG_EXAMPLE_CONNECT_IPV6=n'
}

https_request() {
    example https-request protocols/https_request \
        'CONFIG_EXAMPLE_CONNECT_WIFI=y' 'CONFIG_EXAMPLE_CONNECT_ETHERNET=n' \
        'CONFIG_EXAMPLE_WIFI_SSID="esp32sim"' 'CONFIG_EXAMPLE_WIFI_PASSWORD=""' \
        'CONFIG_EXAMPLE_WIFI_AUTH_OPEN=y' 'CONFIG_EXAMPLE_CONNECT_IPV6=n'
}

diag() {
    name=$1
    [ -d "$ROOT/fixtures/diag-fw/$name" ] || { echo "no fixtures/diag-fw/$name" >&2; exit 1; }
    rm -rf "$SRC/diag-$name"
    mkdir -p "$SRC" "$OUT"
    cp -R "$ROOT/fixtures/diag-fw/$name" "$SRC/diag-$name"
    build "diag-$name" "$SRC/diag-$name"
}

targets=${*:-"wifi-station http-server https-request $(ls "$ROOT/fixtures/diag-fw" 2>/dev/null | sed 's|^|diag-fw/|' | tr '\n' ' ')"}
for t in $targets; do
    case "$t" in
        wifi-station) wifi_station ;;
        http-server) http_server ;;
        https-request) https_request ;;
        diag-fw/*) diag "${t#diag-fw/}" ;;
        *) echo "unknown fixture $t" >&2; exit 2 ;;
    esac
done
