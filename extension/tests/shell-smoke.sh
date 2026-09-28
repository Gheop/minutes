#!/bin/bash
# Loads the extension into a GNOME Shell with no screen, in a D-Bus session and
# config of its own, and checks that it turns on without an error.
#
#     extension/tests/shell-smoke.sh
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
uuid=minutes@gheop.github
tmp=$(mktemp -d)
# The document portal of the test session mounts itself in the runtime dir.
trap 'fusermount3 -u "$tmp/runtime/doc" 2>/dev/null || fusermount -u "$tmp/runtime/doc" 2>/dev/null || true; rm -rf "$tmp" 2>/dev/null || { sleep 1; rm -rf "$tmp"; }' EXIT

gnome-extensions pack --force --podir="$here/po" --extra-source=status.js -o "$tmp" "$here/$uuid" >/dev/null
export XDG_DATA_HOME=$tmp/data XDG_CONFIG_HOME=$tmp/config XDG_CACHE_HOME=$tmp/cache XDG_STATE_HOME=$tmp/state
export XDG_RUNTIME_DIR=$tmp/runtime
mkdir -p "$XDG_DATA_HOME/gnome-shell/extensions/$uuid" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
unzip -q "$tmp/$uuid.shell-extension.zip" -d "$XDG_DATA_HOME/gnome-shell/extensions/$uuid"

dbus-run-session -- bash -c '
    set -euo pipefail
    gsettings set org.gnome.shell disable-user-extensions false
    gsettings set org.gnome.shell enabled-extensions "[\"'"$uuid"'\"]"
    gnome-shell --headless --wayland --virtual-monitor 1280x800 >"$XDG_RUNTIME_DIR/shell.log" 2>&1 &
    shell=$!
    for _ in $(seq 1 60); do
        gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
            --method org.gnome.Shell.Extensions.GetExtensionInfo '"$uuid"' >"$XDG_RUNTIME_DIR/info" 2>/dev/null \
            && grep -qE "'"'"'state'"'"': <1(\.0)?>" "$XDG_RUNTIME_DIR/info" && break
        # Right as the shell starts, the state can read as an error for a
        # moment: only one that lasts counts.
        sleep 1
    done
    info=$(cat "$XDG_RUNTIME_DIR/info" 2>/dev/null || true)
    # What the extension does just after it starts (idle callbacks) too.
    sleep 3
    kill "$shell" 2>/dev/null || true
    wait "$shell" 2>/dev/null || true
    state=$(grep -o "'"'"'state'"'"': <[0-9.]*>" <<<"$info" | grep -o "[0-9.]*" || true)
    error=$(grep -o "'"'"'error'"'"': <[^>]*>" <<<"$info" || true)
    errors=$(grep -A3 "JS ERROR" "$XDG_RUNTIME_DIR/shell.log" | grep -F "$1" || true)
    if [ -n "$errors" ]; then
        echo "$1: active, but with JavaScript errors:" >&2
        grep -B1 -A3 "JS ERROR" "$XDG_RUNTIME_DIR/shell.log" | head -20 >&2
        exit 1
    elif [ "$state" = "1.0" ] || [ "$state" = "1" ]; then
        echo "$1: active in GNOME Shell $(gnome-shell --version | cut -d" " -f3)"
    else
        echo "$1: not active (state ${state:-unknown}) $error" >&2
        grep -iE "minutes|JS ERROR|error" "$XDG_RUNTIME_DIR/shell.log" | head -20 >&2 || true
        exit 1
    fi
' _ "$uuid" 2>"$tmp/session.log" || { tail -30 "$tmp/session.log" >&2; exit 1; }
