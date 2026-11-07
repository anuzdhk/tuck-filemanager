# Packaging Tuck

Tuck needs **GTK ≥ 4.16** and **libadwaita ≥ 1.6** at run time. That means:

| System | Supported | Package |
|---|---|---|
| Debian 13 (trixie) and newer | ✅ | `.deb` |
| Ubuntu 25.04 and newer (and derivatives) | ✅ | `.deb` |
| Ubuntu 24.04 LTS and older | ❌ GTK 4.14 / libadwaita 1.5 are too old | — |
| Fedora 41 and newer | ✅ | `.rpm` |
| openSUSE Tumbleweed | ✅ | `.rpm` (built on Fedora; or build the spec on OBS) |
| openSUSE Leap 15 | ❌ too old | — |
| Arch Linux and derivatives | ✅ | `PKGBUILD` (`.pkg.tar.zst`) |
| Anything else with a new enough glibc | ✅ | AppImage |

All packages install the same files: `/usr/bin/tuck`, the desktop entry
(`io.github.anuzdhk.Tuck.desktop`, which also registers Tuck for folders),
AppStream metadata, the app icon (SVG and 16–512 px PNG), licences and README.

## Releasing

```bash
packaging/bump-version.sh 0.2.0 "One-line summary"
git commit -am "Release 0.2.0" && git tag v0.2.0 && git push && git push origin v0.2.0
```

`bump-version.sh` writes the version into `Cargo.toml`/`Cargo.lock`, the RPM
spec (with a changelog entry), the PKGBUILD and the AppStream release list.
Pushing the tag runs `.github/workflows/release.yml`, which checks the
versions agree, builds every package in a container for its family, writes
`SHA256SUMS`, and publishes a GitHub release with all of them. Tuck's
**Preferences › Updates** reads that release, picks the package for the
user's system and verifies it against `SHA256SUMS`.

## Each format

### From source: `install.sh`

```bash
./install.sh --user            # just you, in ~/.local (no sudo)
sudo ./install.sh              # everyone, in /usr/local
./install.sh --user --set-default   # also open folders with Tuck
./install.sh --user --uninstall
```

`--destdir DIR --prefix /usr --no-build` stages an install for packagers.
Every installed file is listed in `share/tuck/install-manifest`, and
`--uninstall` removes exactly those. Outside `/usr`, the menu entry gets the
full path to the binary, so the app is found even if `~/.local/bin` isn't on
`PATH`.

Tested: staged install and uninstall, and a real `--user` install with
`--set-default` into a fresh home folder (GIO then reports Tuck as the
default for `inode/directory`).

### Debian / Ubuntu: `cargo deb -p tuck`

Configured in `crates/tuck/Cargo.toml` (`[package.metadata.deb]`). Library
dependencies are worked out from the binary, plus explicit minimums for GTK
and libadwaita. Recommends `gvfs-backends`, `gvfs-fuse`, `git`, `xdg-utils`.

Tested: built here, contents and control fields inspected, the extracted
binary runs, the bundled desktop and AppStream files validate, and
`apt-get install -s` resolves every dependency on Ubuntu 26.04.

### Fedora / openSUSE: `packaging/rpm/tuck.spec`

```bash
packaging/rpm/make-sources.sh ~/rpmbuild/SOURCES   # source + vendored crates
rpmbuild -ba packaging/rpm/tuck.spec
```

Builds offline from the vendored crates, so it also works in mock, COPR and
OBS. BuildRequires use `pkgconfig(...)` names, which both Fedora and openSUSE
provide.

Tested: the source and vendor tarballs, and the spec's offline build steps
run by hand (no network, empty cargo cache). `rpmbuild` itself only runs in
the release workflow; it isn't available on the development machine.

### AppImage: `packaging/appimage/build.sh`

Uses quick-sharun from pkgforge-dev's Anylinux-AppImages (pinned revision) in an
Arch Linux container. It bundles every library, glibc and its loader included,
so the AppImage doesn't depend on the host's C library and runs on older
distributions too. `.github/workflows/appimage.yml` builds it and checks that it
starts on Ubuntu 22.04, the system the AppImage catalog tests on; run that
workflow by hand to try a change without releasing. When Tuck runs from an
AppImage, the updater offers the AppImage.

GStreamer plugins aren't bundled, so video and audio previews in Quick Look use
the host's GStreamer if present.

### Arch: `packaging/arch/PKGBUILD`

`makepkg -si` in `packaging/arch/`. Builds from the release tag's source
tarball, runs the core tests, and installs with `install.sh`. For the AUR,
replace `sha256sums=('SKIP')` with the tarball's real checksum.

Not yet tested here: no Arch system; the release workflow builds it in an
`archlinux` container. On Arch, Tuck's updater offers the AppImage rather than
this package, since pacman users update with pacman or their AUR helper.

## Default file manager

The desktop entry declares `MimeType=inode/directory;`. To make Tuck *the*
default: **Preferences › Updates › Make Default** (uses GIO, the same as
`xdg-mime default io.github.anuzdhk.Tuck.desktop inode/directory`), or
`install.sh --set-default`.
