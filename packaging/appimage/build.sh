#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build Tuck-VERSION-ARCH.AppImage with quick-sharun (pkgforge-dev's
# Anylinux-AppImages). It bundles every library Tuck uses, glibc and its
# dynamic loader included, so the AppImage doesn't depend on the host's C
# library and runs on old and new distributions alike.
#
#   packaging/appimage/build.sh [OUTDIR]
#
# Run it as root in an Arch Linux container with the build dependencies
# installed (see .github/workflows/appimage.yml): it installs Tuck into /usr
# first, because quick-sharun collects files from the system.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${1:-$ROOT/target/appimage}
ARCH=$(uname -m)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
WORK="$ROOT/target/appimage-work"
# A pinned revision; quick-sharun pins the tools it downloads by checksum.
QS_REV=1a5594c7f47ec5cb46ce70f6e11b5c5781f17f5f

(cd "$ROOT" && cargo build --release --locked -p tuck)
"$ROOT/install.sh" --no-build --prefix /usr

rm -rf "$WORK"
mkdir -p "$WORK" "$OUT"
cd "$WORK"
TOOLS="https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/$QS_REV/useful-tools"
wget -q -O quick-sharun "$TOOLS/quick-sharun.sh"
wget -q -O get-debloated-pkgs "$TOOLS/get-debloated-pkgs.sh"
chmod +x quick-sharun get-debloated-pkgs
# Slimmed-down Mesa (no 165 MB LLVM), ICU and GTK from the same project:
# keeps the AppImage near the old size while still bundling GPU drivers.
./get-debloated-pkgs --add-common --prefer-nano

export ICON=/usr/share/icons/hicolor/256x256/apps/io.github.anuzdhk.Tuck.png
export DESKTOP=/usr/share/applications/io.github.anuzdhk.Tuck.desktop
export OUTPATH="$OUT"
export OUTNAME="Tuck-$VERSION-$ARCH.AppImage"
./quick-sharun /usr/bin/tuck
./quick-sharun --make-appimage
# Starts it under a virtual display and fails the build if it crashes.
./quick-sharun --test "$OUT/$OUTNAME"
echo "Built $OUT/$OUTNAME"
