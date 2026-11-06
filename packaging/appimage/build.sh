#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build Tuck-VERSION-ARCH.AppImage with linuxdeploy and its GTK plugin.
# Build on the oldest system you want to support that still has
# GTK >= 4.16 and libadwaita >= 1.6 (the release workflow uses Debian 13):
# the AppImage carries Tuck's libraries but uses the host's glibc.
#
#   packaging/appimage/build.sh [OUTDIR]
#
# Needs: cargo, the build dependencies from the README, wget or curl, file.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${1:-$ROOT/target/appimage}
ARCH=$(uname -m)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
TOOLS="$ROOT/target/appimage-tools"
APPDIR="$ROOT/target/appimage-AppDir"

fetch() { # fetch URL FILE
    if command -v wget >/dev/null 2>&1; then wget -q -O "$2" "$1"; else curl -fsSL -o "$2" "$1"; fi
}

mkdir -p "$TOOLS" "$OUT"
# Pinned releases of linuxdeploy and its GTK plugin.
LD="$TOOLS/linuxdeploy-$ARCH.AppImage"
PLUGIN="$TOOLS/linuxdeploy-plugin-gtk.sh"
[ -x "$LD" ] || { fetch "https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-$ARCH.AppImage" "$LD"; chmod +x "$LD"; }
[ -x "$PLUGIN" ] || { fetch "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/7a3fbc31a9e5075073ff8790f26effbac5f84453/linuxdeploy-plugin-gtk.sh" "$PLUGIN"; chmod +x "$PLUGIN"; }

(cd "$ROOT" && cargo build --release --locked -p tuck)
rm -rf "$APPDIR"
"$ROOT/install.sh" --no-build --prefix /usr --destdir "$APPDIR" >/dev/null
rm -rf "$APPDIR/usr/share/tuck"

# linuxdeploy can't use FUSE inside containers.
export APPIMAGE_EXTRACT_AND_RUN=1
export DEPLOY_GTK_VERSION=4
export PATH="$TOOLS:$PATH"
export LDAI_OUTPUT="Tuck-$VERSION-$ARCH.AppImage"
cd "$OUT"
"$LD" --appdir "$APPDIR" \
    --plugin gtk \
    --desktop-file "$APPDIR/usr/share/applications/io.github.anuzdhk.Tuck.desktop" \
    --icon-file "$ROOT/data/icons/app/hicolor/256x256/apps/io.github.anuzdhk.Tuck.png" \
    --output appimage
echo "Built $OUT/$LDAI_OUTPUT"
