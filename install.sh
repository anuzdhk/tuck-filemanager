#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build and install Tuck from source.
#
#   ./install.sh                  build, then install for everyone (needs sudo)
#   ./install.sh --user           build, then install just for you (~/.local)
#   ./install.sh --set-default    also make Tuck open folders (your account)
#   ./install.sh --uninstall      remove what an earlier install put in place
#
# Options: --prefix DIR (default /usr/local, or ~/.local with --user),
#          --destdir DIR (stage into DIR, for packaging), --no-build.
set -eu

APP_ID=io.github.anuzdhk.Tuck
HERE=$(cd "$(dirname "$0")" && pwd)
PREFIX=""
DESTDIR=""
USER_INSTALL=0
BUILD=1
UNINSTALL=0
SET_DEFAULT=0

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --user) USER_INSTALL=1 ;;
        --prefix) [ $# -ge 2 ] || die "--prefix needs a folder"; PREFIX=$2; shift ;;
        --prefix=*) PREFIX=${1#*=} ;;
        --destdir) [ $# -ge 2 ] || die "--destdir needs a folder"; DESTDIR=$2; shift ;;
        --destdir=*) DESTDIR=${1#*=} ;;
        --no-build) BUILD=0 ;;
        --uninstall) UNINSTALL=1 ;;
        --set-default) SET_DEFAULT=1 ;;
        -h|--help) sed -n '3,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
    shift
done

if [ -z "$PREFIX" ]; then
    if [ "$USER_INSTALL" = 1 ]; then PREFIX="$HOME/.local"; else PREFIX=/usr/local; fi
fi
ROOT="$DESTDIR$PREFIX"
BIN="$ROOT/bin"
SHARE="$ROOT/share"
MANIFEST="$SHARE/tuck/install-manifest"

refresh_caches() {
    # Only for a real install; packagers run these in their own hooks.
    [ -z "$DESTDIR" ] || return 0
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$SHARE/applications" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f "$SHARE/icons/hicolor" 2>/dev/null || true
}

if [ "$UNINSTALL" = 1 ]; then
    [ -f "$MANIFEST" ] || die "no install found in $PREFIX (looked for $MANIFEST)"
    while IFS= read -r f; do
        [ -n "$f" ] && rm -f "$DESTDIR$f"
    done < "$MANIFEST"
    rm -f "$MANIFEST"
    rmdir "$SHARE/tuck" "$SHARE/doc/tuck" "$SHARE/licenses/tuck" 2>/dev/null || true
    refresh_caches
    say "Tuck was removed from $PREFIX."
    say "Your settings are kept in ~/.config/tuck; delete that folder too if you like."
    exit 0
fi

# Somewhere we can write?
mkdir -p "$ROOT" 2>/dev/null || true
if [ ! -w "$ROOT" ]; then
    die "can't write to $ROOT. Run with sudo for a system-wide install, or use --user."
fi

if [ "$BUILD" = 1 ]; then
    command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust from https://rustup.rs or your distribution."
    if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}" ]; then
        # Don't build as root: target/ would end up owned by root.
        [ -x "$HERE/target/release/tuck" ] || die "build first as yourself (cargo build --release), then run: sudo ./install.sh --no-build"
        say "Using the existing build in target/release (not building as root)."
    else
        say "Building Tuck (release)…"
        (cd "$HERE" && cargo build --release --locked)
    fi
fi
[ -x "$HERE/target/release/tuck" ] || die "target/release/tuck is missing; build with: cargo build --release"

installed=""
put() { # put MODE SRC DEST
    install -D -m "$1" "$2" "$3"
    installed="$installed
${3#"$DESTDIR"}"
}

put 0755 "$HERE/target/release/tuck" "$BIN/tuck"
put 0644 "$HERE/data/desktop/$APP_ID.desktop" "$SHARE/applications/$APP_ID.desktop"
if [ "$PREFIX" != /usr ]; then
    # Outside /usr the bin folder may not be on PATH, and GIO hides apps whose
    # program it can't find: point the menu entry at the exact binary.
    sed -i "s|^Exec=tuck |Exec=\"$PREFIX/bin/tuck\" |" "$SHARE/applications/$APP_ID.desktop"
fi
put 0644 "$HERE/data/desktop/$APP_ID.metainfo.xml" "$SHARE/metainfo/$APP_ID.metainfo.xml"
put 0644 "$HERE/data/icons/app/$APP_ID.svg" "$SHARE/icons/hicolor/scalable/apps/$APP_ID.svg"
for size in 16 24 32 48 64 128 256 512; do
    put 0644 "$HERE/data/icons/app/hicolor/${size}x${size}/apps/$APP_ID.png" "$SHARE/icons/hicolor/${size}x${size}/apps/$APP_ID.png"
done
put 0644 "$HERE/LICENSE" "$SHARE/licenses/tuck/LICENSE"
for l in "$HERE"/LICENSES/*; do
    put 0644 "$l" "$SHARE/licenses/tuck/$(basename "$l")"
done
put 0644 "$HERE/README.md" "$SHARE/doc/tuck/README.md"

# Record what went where, so --uninstall removes exactly that.
mkdir -p "$(dirname "$MANIFEST")"
printf '%s\n' "$installed" | sed '/^$/d' > "$MANIFEST"
refresh_caches

say "Tuck is installed in $PREFIX."
if [ "$USER_INSTALL" = 1 ]; then
    case ":$PATH:" in
        *":$BIN:"*) ;;
        *) say "Note: $BIN isn't on your PATH; Tuck still starts from your app menu." ;;
    esac
fi

if [ "$SET_DEFAULT" = 1 ]; then
    if [ -n "$DESTDIR" ]; then
        say "Skipping --set-default for a staged (--destdir) install."
    elif ! command -v xdg-mime >/dev/null 2>&1; then
        say "xdg-mime isn't installed; set Tuck as the default in Preferences › Updates instead."
    else
        # Defaults are per account: under sudo, set it for the person who ran sudo.
        as_user() { if [ -n "${SUDO_USER:-}" ]; then sudo -u "$SUDO_USER" -H "$@"; else "$@"; fi; }
        as_user sh -c 'mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}"'
        for t in inode/directory application/x-gnome-saved-search; do
            as_user xdg-mime default "$APP_ID.desktop" "$t" || true
        done
        # GIO is what GNOME and GTK apps ask; xdg-mime's own query has its
        # own rules and can disagree, so prefer gio when it's there.
        if command -v gio >/dev/null 2>&1; then
            now=$(as_user gio mime inode/directory 2>/dev/null | sed -n '1s/.*: //p')
        else
            now=$(as_user xdg-mime query default inode/directory)
        fi
        if [ "$now" = "$APP_ID.desktop" ]; then
            say "Tuck now opens folders by default. To undo, pick another app in your desktop's settings."
        else
            say "Couldn't make Tuck the default; use Preferences › Updates in Tuck instead."
        fi
    fi
fi
