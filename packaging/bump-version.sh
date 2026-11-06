#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Set the release version everywhere it's written down:
#   packaging/bump-version.sh 0.2.0 "One-line summary of the release"
# Updates Cargo.toml (and Cargo.lock), the RPM spec, the PKGBUILD and the
# AppStream release list. Then commit, tag v0.2.0 and push the tag: the
# release workflow builds the packages and publishes them.
set -eu
[ $# -ge 1 ] || { echo "usage: $0 VERSION [SUMMARY]" >&2; exit 1; }
V=$1
SUMMARY=${2:-"Release $V"}
case "$V" in *[!0-9.]*|"") echo "version must look like 1.2.3" >&2; exit 1 ;; esac
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
DATE=$(date +%Y-%m-%d)
SPECDATE=$(LC_ALL=C date "+%a %b %d %Y")

sed -i "0,/^version = \".*\"/s//version = \"$V\"/" Cargo.toml
cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null
sed -i "s/^Version: .*/Version:        $V/; s/^Release: .*/Release:        1%{?dist}/" packaging/rpm/tuck.spec
sed -i "/^%changelog/a * $SPECDATE anuzdhk <95576641+anuzdhk@users.noreply.github.com> - $V-1\\n- $SUMMARY\\n" packaging/rpm/tuck.spec
sed -i "s/^pkgver=.*/pkgver=$V/; s/^pkgrel=.*/pkgrel=1/" packaging/arch/PKGBUILD
if ! grep -q "<release version=\"$V\"" data/desktop/io.github.anuzdhk.Tuck.metainfo.xml; then
    sed -i "s|  <releases>|  <releases>\n    <release version=\"$V\" date=\"$DATE\">\n      <description>\n        <p>$SUMMARY</p>\n      </description>\n    </release>|" data/desktop/io.github.anuzdhk.Tuck.metainfo.xml
fi
"$ROOT/packaging/check-version.sh" "$V"
echo "Now: git commit -am \"Release $V\" && git tag v$V && git push && git push origin v$V"
