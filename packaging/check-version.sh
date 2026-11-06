#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Check that every file states the same version (and that it matches VERSION
# or the tag in $GITHUB_REF_NAME, if given).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
cargo=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
spec=$(sed -n 's/^Version: *//p' packaging/rpm/tuck.spec)
arch=$(sed -n 's/^pkgver=//p' packaging/arch/PKGBUILD)
meta=$(sed -n 's/.*<release version="\([^"]*\)".*/\1/p' data/desktop/io.github.anuzdhk.Tuck.metainfo.xml | head -n1)
want=${1:-${GITHUB_REF_NAME:-$cargo}}
want=${want#v}
ok=1
for pair in "Cargo.toml:$cargo" "tuck.spec:$spec" "PKGBUILD:$arch" "metainfo (newest release):$meta"; do
    if [ "${pair#*:}" != "$want" ]; then
        echo "version mismatch: ${pair%%:*} says ${pair#*:}, expected $want" >&2
        ok=0
    fi
done
[ "$ok" = 1 ] && echo "All files say version $want."
[ "$ok" = 1 ]
