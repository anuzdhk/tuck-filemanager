#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Make the two tarballs tuck.spec builds from, in DIR (default: ~/rpmbuild/SOURCES):
#   tuck-VERSION.tar.gz         the source tree at HEAD (git archive)
#   tuck-VERSION-vendor.tar.xz  every crate dependency (cargo vendor), for offline builds
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${1:-$HOME/rpmbuild/SOURCES}
VERSION=$(sed -n 's/^Version: *//p' "$ROOT/packaging/rpm/tuck.spec")
mkdir -p "$OUT"
cd "$ROOT"
git archive --format=tar.gz --prefix="tuck-$VERSION/" -o "$OUT/tuck-$VERSION.tar.gz" HEAD
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo vendor --quiet --locked --versioned-dirs "$tmp/vendor" >/dev/null
tar -C "$tmp" -cJf "$OUT/tuck-$VERSION-vendor.tar.xz" vendor
echo "Wrote $OUT/tuck-$VERSION.tar.gz and $OUT/tuck-$VERSION-vendor.tar.xz"
