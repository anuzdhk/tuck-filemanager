# SPDX-License-Identifier: GPL-3.0-or-later
# Fedora and openSUSE. Builds offline from the source tarball plus a
# vendored-crates tarball; packaging/rpm/make-sources.sh creates both.
Name:           tuck
Version:        0.1.2
Release:        1%{?dist}
Summary:        Fast, friendly file manager
# Tuck is GPL-3.0-or-later; bundled icons are MIT (Tabler) and Apache-2.0
# (Material Symbols); vendored Rust crates are MIT/Apache-2.0/BSD/Zlib/Unicode.
License:        GPL-3.0-or-later AND MIT AND Apache-2.0
URL:            https://github.com/anuzdhk/kivo-filemanager
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk4) >= 4.16
BuildRequires:  pkgconfig(libadwaita-1) >= 1.6
BuildRequires:  pkgconfig(gtksourceview-5)
BuildRequires:  pkgconfig(poppler-glib)
BuildRequires:  pkgconfig(libarchive)
BuildRequires:  pkgconfig(vte-2.91-gtk4)
BuildRequires:  desktop-file-utils

Requires:       hicolor-icon-theme
Recommends:     gvfs
Recommends:     git
Recommends:     xdg-utils
Suggests:       flatpak

%description
Tuck is a fast, friendly file manager: icons, compact and details views,
tabs and split view, Quick Look previews with Space, instant search, safe
double-click archive extraction, built-in app installation, tags and
service menus shared with Dolphin, git status and a built-in terminal.

%prep
%autosetup -n %{name}-%{version} -a 1
mkdir -p .cargo
cat > .cargo/config.toml <<'CFG'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CFG

%build
export RUSTFLAGS="%{?build_rustflags}"
cargo build --release --locked --offline -p tuck

%install
./install.sh --no-build --prefix %{_prefix} --destdir %{buildroot}
# Licences and docs are installed by %%license / %%doc below.
rm -rf %{buildroot}%{_datadir}/licenses/tuck %{buildroot}%{_datadir}/doc/tuck %{buildroot}%{_datadir}/tuck

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.anuzdhk.Tuck.desktop
%{buildroot}%{_bindir}/tuck --version

%files
%license LICENSE LICENSES/*
%doc README.md
%{_bindir}/tuck
%{_datadir}/applications/io.github.anuzdhk.Tuck.desktop
%{_datadir}/metainfo/io.github.anuzdhk.Tuck.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/io.github.anuzdhk.Tuck.*

%changelog
* Fri Nov 06 2026 anuzdhk <95576641+anuzdhk@users.noreply.github.com> - 0.1.2-1
- Cleaner, drive usage in the sidebar, Storage in Preferences, resizable sidebar, Quick Look header fix

* Fri Nov 06 2026 anuzdhk <95576641+anuzdhk@users.noreply.github.com> - 0.1.1-1
- AppImage now follows Tuck's own styling and runs natively on Wayland; one-line curl installer; debug packages no longer published

* Fri Nov 06 2026 anuzdhk <95576641+anuzdhk@users.noreply.github.com> - 0.1.0-1
- First release
