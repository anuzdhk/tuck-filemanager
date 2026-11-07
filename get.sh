#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Install the latest Tuck release:
#
#   curl -fsSL https://raw.githubusercontent.com/anuzdhk/tuck-filemanager/feat/strata/get.sh | sh
#
# Picks the package for your system (.deb, .rpm, Arch package, or the
# AppImage), checks it against the release's SHA256SUMS, and installs it with
# your package manager (asks for your password through sudo). The AppImage
# installs just for you, without sudo.
#
# Options (after `sh -s --` when piping):
#   --appimage   use the AppImage even if a package fits your system
#   --dry-run    show what would be downloaded and installed, then stop
set -eu

REPO=anuzdhk/tuck-filemanager
# TUCK_RELEASE_API and TUCK_OS_RELEASE are for testing this script.
API="${TUCK_RELEASE_API:-https://api.github.com/repos/$REPO/releases/latest}"
OS_RELEASE="${TUCK_OS_RELEASE:-/etc/os-release}"
FORCE_APPIMAGE=0
DRY_RUN=0

for a in "$@"; do
    case "$a" in
        --appimage) FORCE_APPIMAGE=1 ;;
        --dry-run) DRY_RUN=1 ;;
        -h|--help) sed -n '3,15p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "get.sh: unknown option $a" >&2; exit 1 ;;
    esac
done

say() { printf '%s\n' "$*"; }
die() { printf 'Tuck installer: %s\n' "$*" >&2; exit 1; }

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
    fetch_text() { curl -fsSL -H "Accept: application/vnd.github+json" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
    fetch_text() { wget -q -O - --header="Accept: application/vnd.github+json" "$1"; }
else
    die "needs curl or wget."
fi
command -v sha256sum >/dev/null 2>&1 || die "needs sha256sum (coreutils)."

# ---------------------------------------------------------- this system
case "$(uname -m)" in
    x86_64|amd64) ARCH_WORDS="x86_64 amd64" ;;
    aarch64|arm64) ARCH_WORDS="aarch64 arm64" ;;
    *) die "there's no Tuck build for $(uname -m) yet." ;;
esac

FAMILY=unknown
if [ -r "$OS_RELEASE" ]; then
    # shellcheck disable=SC1091
    ids=$(. "$OS_RELEASE" && printf '%s %s' "${ID:-}" "${ID_LIKE:-}")
    for id in $ids; do
        case "$id" in
            debian|ubuntu) FAMILY=debian; break ;;
            fedora|rhel|centos) FAMILY=fedora; break ;;
            suse|sles|opensuse*) FAMILY=suse; break ;;
            arch) FAMILY=arch; break ;;
        esac
    done
fi
[ "$FORCE_APPIMAGE" = 1 ] && FAMILY=appimage

case "$FAMILY" in
    debian) PATTERN='\.deb$' ;;
    fedora|suse) PATTERN='\.rpm$' ;;
    arch) PATTERN='\.pkg\.tar\.(zst|xz)$' ;;
    *) PATTERN='\.AppImage$'; FAMILY=appimage ;;
esac

# -------------------------------------------------------- the release
say "Looking up the latest Tuck release…"
JSON=$(fetch_text "$API") || die "couldn't get the latest release from GitHub (none published yet, or no internet connection)."
TAG=$(printf '%s' "$JSON" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
[ -n "$TAG" ] || die "no release has been published yet."
URLS=$(printf '%s' "$JSON" | sed -n 's/.*"browser_download_url": *"\([^"]*\)".*/\1/p')

pick() { # pick PATTERN: first asset matching it and this CPU (or no CPU in the name)
    for u in $URLS; do
        n=$(basename "$u")
        printf '%s' "$n" | grep -Eq "$1" || continue
        printf '%s' "$n" | grep -q '\.src\.rpm$' && continue
        printf '%s' "$n" | grep -Eq -- '-debug(info|source)?-' && continue
        mine=0; anyarch=0
        for w in $ARCH_WORDS; do printf '%s' "$n" | grep -q "$w" && mine=1; done
        for w in x86_64 amd64 aarch64 arm64 i686 i386 armhf; do printf '%s' "$n" | grep -q "$w" && anyarch=1; done
        if [ "$mine" = 1 ] || [ "$anyarch" = 0 ]; then printf '%s' "$u"; return 0; fi
    done
    return 1
}

URL=$(pick "$PATTERN") || {
    say "No $FAMILY package in $TAG for this system; using the AppImage."
    FAMILY=appimage
    URL=$(pick '\.AppImage$') || die "$TAG has nothing for $(uname -m)."
}
SUMS_URL=$(for u in $URLS; do case "$u" in */SHA256SUMS) printf '%s' "$u" ;; esac; done)
FILE=$(basename "$URL")

say "Tuck $TAG: $FILE"
if [ "$DRY_RUN" = 1 ]; then
    say "Would download: $URL"
    [ -n "$SUMS_URL" ] && say "Would verify against: $SUMS_URL" || say "This release has no SHA256SUMS; the download would not be verified."
    case "$FAMILY" in
        debian) say "Would run: sudo apt-get install ./$FILE" ;;
        fedora) say "Would run: sudo dnf install ./$FILE" ;;
        suse) say "Would run: sudo zypper install --allow-unsigned-rpm ./$FILE" ;;
        arch) say "Would run: sudo pacman -U ./$FILE" ;;
        *) say "Would install to ~/Applications/Tuck.AppImage and add Tuck to your app menu" ;;
    esac
    exit 0
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM
say "Downloading…"
fetch "$URL" "$TMP/$FILE" || die "the download failed."
if [ -n "$SUMS_URL" ]; then
    fetch "$SUMS_URL" "$TMP/SHA256SUMS" || die "couldn't download SHA256SUMS."
    grep " \*\{0,1\}$FILE\$" "$TMP/SHA256SUMS" > "$TMP/expected" || die "SHA256SUMS doesn't list $FILE."
    (cd "$TMP" && sha256sum -c --quiet expected) || die "the download doesn't match its checksum. Nothing was installed."
    say "Checksum verified."
else
    say "Warning: this release has no SHA256SUMS, so the download can't be verified."
fi

# ------------------------------------------------------------- install
SUDO=""
if [ "$FAMILY" != appimage ] && [ "$(id -u)" != 0 ]; then
    command -v sudo >/dev/null 2>&1 || die "needs sudo to install $FILE (or run as root)."
    SUDO=sudo
fi

case "$FAMILY" in
    debian) $SUDO apt-get install -y "$TMP/$FILE" ;;
    fedora) $SUDO dnf install -y "$TMP/$FILE" ;;
    suse) $SUDO zypper --non-interactive install --allow-unsigned-rpm "$TMP/$FILE" ;;
    arch) $SUDO pacman -U --noconfirm "$TMP/$FILE" ;;
    *)
        APPS="$HOME/Applications"
        DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
        mkdir -p "$APPS" "$DATA/applications" "$DATA/icons/hicolor/scalable/apps"
        install -m 0755 "$TMP/$FILE" "$APPS/Tuck.AppImage"
        fetch "https://raw.githubusercontent.com/$REPO/$TAG/data/icons/app/io.github.anuzdhk.Tuck.svg" \
            "$DATA/icons/hicolor/scalable/apps/io.github.anuzdhk.Tuck.svg" || true
        cat > "$DATA/applications/io.github.anuzdhk.Tuck.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Tuck
GenericName=File Manager
Comment=Browse, organise and find your files
Exec="$APPS/Tuck.AppImage" %U
Icon=io.github.anuzdhk.Tuck
Terminal=false
Categories=GTK;System;FileTools;FileManager;
MimeType=inode/directory;
EOF
        command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$DATA/applications" 2>/dev/null || true
        say "Installed to $APPS/Tuck.AppImage."
        ;;
esac

if [ "$FAMILY" = appimage ]; then say "Done. Open Tuck from your app menu."; else say "Done. Open Tuck from your app menu, or run: tuck"; fi
say "To make Tuck open folders everywhere: Tuck › Preferences › Updates › Make Default."
