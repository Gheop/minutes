#!/bin/bash
# Packs the extension with its translations into minutes@gheop.github.shell-extension.zip.
#
#     extension/build.sh                 # the zip, next to this script
#     extension/build.sh --install       # and install it for your user
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
gnome-extensions pack --force --podir="$here/po" --extra-source=status.js -o "$here" "$here/minutes@gheop.github"
echo "$here/minutes@gheop.github.shell-extension.zip"
if [ "${1:-}" = "--install" ]; then
    gnome-extensions install --force "$here/minutes@gheop.github.shell-extension.zip"
    echo "Installed. Log out and back in, then: gnome-extensions enable minutes@gheop.github"
fi
