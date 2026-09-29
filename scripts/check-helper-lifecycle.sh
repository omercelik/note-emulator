#!/bin/sh
# PKG-02 live check. Run as the intended user in Terminal after installing the
# helper; sudo asks for an administrator password once. Leaves it installed.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
APP=${NOTE_EMU_APP:-"$ROOT/dist/NOTE Emulator-pkg02.app"}
INSTALLER="$APP/Contents/Resources/install-helper.sh"
EMU="$APP/Contents/MacOS/note-emu"
FIXTURE="$ROOT/third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"
UID_NUMBER=$(id -u)
TOOL="/Library/PrivilegedHelperTools/dev.note-emulator.helper.$UID_NUMBER"
STATE="/Library/Application Support/NOTE Emulator/helper/$UID_NUMBER"
SOCKET="/var/run/note-emulator-$UID_NUMBER.sock"
LABEL="dev.note-emulator.helper.$UID_NUMBER"
LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT

[ -x "$INSTALLER" ] && [ -x "$EMU" ] && [ -f "$FIXTURE" ] || {
    echo "bundle or NOTE4 test fixture missing" >&2; exit 1;
}
[ -S "$SOCKET" ] || { echo "install the helper first" >&2; exit 1; }
if lsof -nP -iTCP@192.168.4.1:80 -sTCP:LISTEN 2>/dev/null | tail -n +2 | grep -q .; then
    echo "stop the emulator using 192.168.4.1:80 first" >&2; exit 1
fi

probe() {
    "$EMU" --profile note4 --firmware "$FIXTURE" --setup-address --seconds 1 >"$LOG" 2>&1
}
probe_after_start() {
    i=0
    while [ "$i" -lt 20 ]; do
        if probe; then return 0; fi
        sleep 0.1
        i=$((i + 1))
    done
    return 1
}
wait_socket() {
    i=0
    while [ ! -S "$SOCKET" ] && [ "$i" -lt 50 ]; do
        sleep 0.1
        i=$((i + 1))
    done
    [ -S "$SOCKET" ]
}
no_endpoint() {
    ! ifconfig lo0 | grep -q 'inet 192\.168\.4\.1 '
    ! lsof -nP -iTCP@192.168.4.1:80 -sTCP:LISTEN 2>/dev/null | tail -n +2 | grep -q .
}

sudo -v
sudo "$TOOL" cancel --state-dir "$STATE" >/dev/null
if probe || ! grep -q AuthorizationCancelled "$LOG"; then
    sudo "$TOOL" authorize --state-dir "$STATE" >/dev/null
    echo "FAIL  cancelled helper granted a lease or returned the wrong error" >&2; exit 1
fi
no_endpoint || { echo "FAIL  cancellation left a setup endpoint" >&2; exit 1; }
echo "PASS  cancellation denies a lease without adding an alias"

sudo "$TOOL" authorize --state-dir "$STATE" >/dev/null
probe || { tail -8 "$LOG" >&2; echo "FAIL  reauthorization" >&2; exit 1; }
grep -q 'setup address http://192.168.4.1/' "$LOG"
no_endpoint || { echo "FAIL  normal exit left a setup endpoint" >&2; exit 1; }
echo "PASS  reauthorization grants and releases a listener"

sudo launchctl kickstart -k "system/$LABEL"
wait_socket || { echo "FAIL  helper did not restart" >&2; exit 1; }
probe_after_start || { tail -8 "$LOG" >&2; echo "FAIL  lease after helper restart" >&2; exit 1; }
no_endpoint || { echo "FAIL  restart test left a setup endpoint" >&2; exit 1; }
echo "PASS  helper restart accepts a new lease"

sh "$INSTALLER" uninstall
[ ! -S "$SOCKET" ] && [ ! -e "$TOOL" ] && ! launchctl print "system/$LABEL" >/dev/null 2>&1 || {
    echo "FAIL  uninstall left the service, socket, or executable" >&2; exit 1;
}
no_endpoint || { echo "FAIL  uninstall left a setup endpoint" >&2; exit 1; }
echo "PASS  uninstall cleaned up the service and socket"

sh "$INSTALLER" install
wait_socket || { echo "FAIL  reinstall did not start the helper" >&2; exit 1; }
probe_after_start || { tail -8 "$LOG" >&2; echo "FAIL  lease after reinstall" >&2; exit 1; }
no_endpoint || { echo "FAIL  reinstall test left a setup endpoint" >&2; exit 1; }
echo "PASS  reinstall restored the service; helper remains installed"
