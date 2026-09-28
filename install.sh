#!/bin/bash
# Installs Minutes for your user, in ~/.local: the app with its launcher, icon
# and translations, and the GNOME Shell extension.
#
#     ./install.sh                         # build (CPU) and install
#     ./install.sh --features vulkan       # whisper on the GPU (also the better choice on NVIDIA)
#     ./install.sh --features cuda         # whisper on an NVIDIA GPU through CUDA; CUDAARCHS=86 for an RTX 30 series
#     ./install.sh --autostart             # also start Minutes in the background at login, to notice calls
#     ./install.sh --uninstall             # remove all of it; meetings and settings stay
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
app_id=io.github.gheop.Minutes
bin=$HOME/.local/bin
# Next to bin/, where the binary looks for the libraries it comes with.
lib=$HOME/.local/lib/minutes
share=${XDG_DATA_HOME:-$HOME/.local/share}
autostart=${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$app_id.desktop

features=()
with_autostart=false
while [ $# -gt 0 ]; do
    case $1 in
        --features) features=(--features "$2"); shift ;;
        --autostart) with_autostart=true ;;
        --uninstall)
            rm -rf "$lib"
            rm -f "$bin/minutes" "$share/applications/$app_id.desktop" "$share/metainfo/$app_id.metainfo.xml" \
                "$share/icons/hicolor/scalable/apps/$app_id.svg" "$share/icons/hicolor/symbolic/apps/$app_id-symbolic.svg" \
                "$autostart"
            find "$share/locale" -name minutes.mo -delete 2>/dev/null || true
            gnome-extensions uninstall minutes@gheop.github 2>/dev/null || true
            echo "Minutes is uninstalled. Meetings stay in ~/Documents/Meetings."
            exit 0 ;;
        *) echo "unknown option: $1 (see the top of $0)" >&2; exit 2 ;;
    esac
    shift
done

cd "$here"
# The whole workspace, so the features match every earlier build and
# whisper.cpp is not compiled again.
cargo build --release --workspace "${features[@]}"

install -Dm755 target/release/minutes "$bin/minutes"
# The WebGPU library of the Vulkan build.
if [ -e target/release/libwebgpu_dawn.so ]; then
    install -Dm644 target/release/libwebgpu_dawn.so "$lib/libwebgpu_dawn.so"
fi
# The launcher runs the installed binary, whatever the session's PATH.
sed "s|^Exec=minutes|Exec=$bin/minutes|" app/data/$app_id.desktop > "$share/applications/$app_id.desktop"
install -Dm644 app/data/$app_id.metainfo.xml "$share/metainfo/$app_id.metainfo.xml"
install -Dm644 app/data/icons/hicolor/scalable/apps/$app_id.svg "$share/icons/hicolor/scalable/apps/$app_id.svg"
install -Dm644 app/data/icons/hicolor/symbolic/apps/$app_id-symbolic.svg \
    "$share/icons/hicolor/symbolic/apps/$app_id-symbolic.svg"
for lang in $(cat app/po/LINGUAS); do
    mkdir -p "$share/locale/$lang/LC_MESSAGES"
    msgfmt -o "$share/locale/$lang/LC_MESSAGES/minutes.mo" "app/po/$lang.po"
done
update-desktop-database -q "$share/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t -f "$share/icons/hicolor" 2>/dev/null || true
extension/build.sh --install >/dev/null

if $with_autostart; then
    mkdir -p "$(dirname "$autostart")"
    cat > "$autostart" <<EOF
[Desktop Entry]
Type=Application
Name=Minutes
Comment=Notices calls and offers to record them
Exec=$bin/minutes --background
Icon=$app_id
NoDisplay=true
X-GNOME-Autostart-enabled=true
EOF
fi

echo "Minutes is installed: find it in the launcher, or run $bin/minutes."
$with_autostart && echo "It starts in the background at login, without a window and without the microphone."
echo "The Shell extension shows up after you log out and back in."
