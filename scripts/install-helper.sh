#!/bin/sh
# Local macOS helper installation for one user. launchd only ever runs a
# root-owned copy of the helper, never the writable app bundle. Installing does
# run this script from the bundle as root, once, after the administrator
# approves; that approval is the trust decision. `codesign --verify` only
# catches a damaged bundle: an ad-hoc signature is not an identity check.
set -eu

ACTION=${1:-}
case "$ACTION" in
    install-root)
        [ "$(id -u)" -eq 0 ] || { echo "install-root requires administrator privileges" >&2; exit 2; }
        UID_NUMBER=${2:-}
        case "$UID_NUMBER" in ''|*[!0-9]*) echo "invalid owner UID" >&2; exit 2 ;; esac
        [ "$UID_NUMBER" -gt 0 ] || { echo "invalid owner UID" >&2; exit 2; }
        ACTION=install
        ROOT_INSTALL=1
        ;;
    plan|install|cancel|uninstall|status)
        [ "$(id -u)" -ne 0 ] || { echo "run as the intended user; this script invokes sudo" >&2; exit 2; }
        UID_NUMBER=$(id -u)
        ROOT_INSTALL=0
        ;;
    *) echo "usage: install-helper.sh plan|install|cancel|uninstall|status" >&2; exit 2 ;;
esac
LABEL="dev.note-emulator.helper.$UID_NUMBER"
PLIST="/Library/LaunchDaemons/$LABEL.plist"
TOOL="/Library/PrivilegedHelperTools/dev.note-emulator.helper.$UID_NUMBER"
STATE="/Library/Application Support/NOTE Emulator/helper/$UID_NUMBER"
SOCKET="/var/run/note-emulator-$UID_NUMBER.sock"
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "$HERE" in
    */Contents/Resources) SOURCE="$HERE/../MacOS/note-net-helper" ;;
    *) SOURCE="$HERE/../dist/NOTE Emulator.app/Contents/MacOS/note-net-helper" ;;
esac

# bootout returns before the old job has gone; bootstrap too early fails with EIO.
wait_unloaded() {
    i=0
    while launchctl print "system/$LABEL" >/dev/null 2>&1 && [ "$i" -lt 50 ]; do
        sleep 0.1
        i=$((i + 1))
    done
}

if [ "$ACTION" = status ]; then
    if [ -S "$SOCKET" ]; then echo "socket: $SOCKET"; else echo "socket absent: $SOCKET"; fi
    launchctl print "system/$LABEL" 2>&1 || true
    exit 0
fi

# An active runtime holds the listeners even if launchd stops the helper.
if [ "$ACTION" != install ]; then
    for ENDPOINT in 192.168.4.1 10.0.2.15; do
        if lsof -nP -iTCP@$ENDPOINT:80 -sTCP:LISTEN 2>/dev/null | tail -n +2 | grep -q .; then
            echo "stop the emulator using $ENDPOINT:80 before $ACTION" >&2
            exit 1
        fi
    done
fi

if [ "$ACTION" = install ] || [ "$ACTION" = plan ]; then
    [ -f "$SOURCE" ] || { echo "bundle helper missing: $SOURCE" >&2; exit 1; }
    codesign --verify --strict "$SOURCE"
    STAGING=$(mktemp)
    trap 'rm -f "$STAGING"' EXIT
    cat > "$STAGING" <<PLIST_CONTENT
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$LABEL</string>
<key>ProgramArguments</key><array>
<string>$TOOL</string><string>run</string>
<string>--owner-uid</string><string>$UID_NUMBER</string>
<string>--socket</string><string>$SOCKET</string>
<string>--state-dir</string><string>$STATE</string>
</array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
</dict></plist>
PLIST_CONTENT
    plutil -lint "$STAGING"
    if [ "$ACTION" = plan ]; then
        echo "signed source: $SOURCE"
        echo "root-owned copy: $TOOL"
        echo "authorization state: $STATE"
        echo "user-only socket: $SOCKET"
        plutil -p "$STAGING"
        exit 0
    fi
    if [ "$ROOT_INSTALL" -eq 0 ]; then
        sudo /bin/sh "$0" install-root "$UID_NUMBER"
        exit $?
    fi
    mkdir -p /Library/PrivilegedHelperTools "$STATE"
    chown root:wheel "$STATE"
    chmod 700 "$STATE"
    install -o root -g wheel -m 755 "$SOURCE" "$TOOL"
    install -o root -g wheel -m 644 "$STAGING" "$PLIST"
    codesign --verify --strict "$TOOL"
    plutil -lint "$PLIST"
    "$TOOL" authorize --state-dir "$STATE"
    launchctl bootout "system/$LABEL" 2>/dev/null || true
    wait_unloaded
    launchctl bootstrap system "$PLIST"
    echo "installed for UID $UID_NUMBER; socket: $SOCKET"
    exit 0
fi

if [ -x "$TOOL" ]; then
    sudo "$TOOL" cancel --state-dir "$STATE"
elif [ "$ACTION" = cancel ]; then
    echo "helper is not installed for UID $UID_NUMBER" >&2
    exit 1
fi
if [ "$ACTION" = cancel ]; then
    echo "authorization cancelled for UID $UID_NUMBER"
    exit 0
fi
sudo launchctl bootout "system/$LABEL" 2>/dev/null || true
wait_unloaded
sudo rm -f "$PLIST" "$SOCKET" "$TOOL"
echo "unregistered for UID $UID_NUMBER (authorization remains cancelled)"
