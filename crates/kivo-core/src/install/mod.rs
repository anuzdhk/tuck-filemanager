// SPDX-License-Identifier: GPL-3.0-or-later
//! App installation, ported from Teku (https://github.com/anuzdhk/teku,
//! GPL-3.0-or-later) and matching its behaviour:
//!
//! - The file type comes from the contents, never the name, so a renamed
//!   file can't fool it. Teku used `file(1)`; this reads the same magic
//!   numbers directly.
//! - The distro family comes from `ID`, then `ID_LIKE`, in /etc/os-release
//!   (falling back to /usr/lib/os-release).
//! - .deb → `pkexec apt-get install` on Debian-based systems; .rpm →
//!   `pkexec dnf install` (Fedora/RHEL) or `pkexec zypper install`
//!   (openSUSE); .AppImage → ~/Applications with a menu entry; .flatpakref →
//!   `flatpak install --from`. Windows and macOS packages get a plain
//!   explanation.
//! - History is a tab-separated log, as in Teku.

pub mod appimage;

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Deb,
    Rpm,
    AppImage,
    FlatpakRef,
    Windows,
    MacOs,
    Unknown,
}

impl FileType {
    /// Teku's `teku_type_label`.
    pub fn label(self) -> &'static str {
        match self {
            FileType::Deb => "Debian package",
            FileType::Rpm => "RPM package",
            FileType::AppImage => "AppImage",
            FileType::FlatpakRef => "Flatpak reference",
            FileType::Windows => "Windows executable",
            FileType::MacOs => "macOS package",
            FileType::Unknown => "unknown",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            FileType::Deb => "deb",
            FileType::Rpm => "rpm",
            FileType::AppImage => "appimage",
            FileType::FlatpakRef => "flatpakref",
            FileType::Windows => "windows",
            FileType::MacOs => "macos",
            FileType::Unknown => "unknown",
        }
    }
}

/// Lowercased extension (Teku's `teku_ext`).
pub fn ext(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.rfind('.') {
        Some(i) => name[i + 1..].to_lowercase(),
        None => String::new(),
    }
}

fn head(path: &Path, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let Ok(mut f) = File::open(path) else { return Vec::new() };
    let mut got = 0;
    while got < n {
        match f.read(&mut buf[got..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => got += k,
        }
    }
    buf.truncate(got);
    buf
}

fn tail(path: &Path, n: u64) -> Vec<u8> {
    let Ok(mut f) = File::open(path) else { return Vec::new() };
    let Ok(len) = f.metadata().map(|m| m.len()) else { return Vec::new() };
    if len < n || f.seek(SeekFrom::Start(len - n)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(n).read_to_end(&mut buf);
    buf
}

/// Teku's `teku_file_type`: content first, extension only as a last resort
/// for foreign-platform files.
pub fn file_type(path: &Path) -> FileType {
    let h = head(path, 4096);
    let e = ext(path);
    // Debian package: an ar archive whose first member is "debian-binary".
    if h.starts_with(b"!<arch>\n") && h.len() >= 8 + 16 && h[8..8 + 13] == *b"debian-binary" {
        return FileType::Deb;
    }
    // RPM lead magic.
    if h.starts_with(&[0xED, 0xAB, 0xEE, 0xDB]) {
        return FileType::Rpm;
    }
    // Windows PE / MS-DOS executable.
    if h.starts_with(b"MZ") {
        return FileType::Windows;
    }
    // macOS installer package (xar archive).
    if h.starts_with(b"xar!") {
        return FileType::MacOs;
    }
    // .msi files are OLE compound documents, like old .doc files.
    if h.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) && e == "msi" {
        return FileType::Windows;
    }
    // AppImage: ELF (type 2) or ISO 9660 (type 1) with "AI\x01"/"AI\x02" at offset 8.
    if h.len() >= 11 && &h[8..10] == b"AI" && (h[10] == 1 || h[10] == 2) {
        return FileType::AppImage;
    }
    // UDIF disk images end with a 512-byte "koly" trailer.
    if tail(path, 512).starts_with(b"koly") {
        return FileType::MacOs;
    }
    // Flatpak reference: a text file with a [Flatpak Ref] group.
    if !h.contains(&0) && std::str::from_utf8(&h).is_ok_and(|t| t.lines().any(|l| l.trim_end() == "[Flatpak Ref]")) {
        return FileType::FlatpakRef;
    }
    match e.as_str() {
        "exe" | "msi" => FileType::Windows,
        "dmg" | "pkg" => FileType::MacOs,
        _ => FileType::Unknown,
    }
}

/// Is this something the install flow handles (including the "this is a
/// Windows/macOS app" explanations)?
pub fn is_installable(path: &Path) -> bool {
    !matches!(file_type(path), FileType::Unknown) || matches!(ext(path).as_str(), "deb" | "rpm" | "appimage" | "flatpakref")
}

// ---------------------------------------------------------------- the OS

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    Debian,
    Fedora,
    Suse,
    Arch,
    Unknown,
}

/// Parse os-release (shell-style KEY=value, optionally quoted).
pub fn parse_os_release(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let v = if (v.starts_with('"') && v.ends_with('"') && v.len() >= 2) || (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2) {
            v[1..v.len() - 1].replace("\\\"", "\"").replace("\\$", "$").replace("\\\\", "\\")
        } else {
            v.to_string()
        };
        out.insert(k.trim().to_string(), v);
    }
    out
}

/// Teku's `teku_os_family`: ID first, then each ID_LIKE word.
pub fn family(release: &HashMap<String, String>) -> OsFamily {
    let ids = release.get("ID").into_iter().chain(release.get("ID_LIKE")).flat_map(|v| v.split_whitespace());
    for id in ids {
        match id {
            "debian" | "ubuntu" => return OsFamily::Debian,
            "fedora" | "rhel" | "centos" => return OsFamily::Fedora,
            "suse" | "sles" => return OsFamily::Suse,
            s if s.starts_with("opensuse") => return OsFamily::Suse,
            "arch" => return OsFamily::Arch,
            _ => {}
        }
    }
    OsFamily::Unknown
}

pub fn read_os_release() -> HashMap<String, String> {
    let file = std::env::var_os("TEKU_OS_RELEASE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/etc/os-release"));
    let text = std::fs::read_to_string(&file).or_else(|_| std::fs::read_to_string("/usr/lib/os-release")).unwrap_or_default();
    parse_os_release(&text)
}

pub fn os_name(release: &HashMap<String, String>) -> String {
    release.get("PRETTY_NAME").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| "Unknown Linux".into())
}

// ---------------------------------------------------------------- routing

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Apt,
    Dnf,
    Zypper,
    AppImage,
    Flatpak,
}

impl Method {
    /// Teku's `method_label`.
    pub fn label(self) -> &'static str {
        match self {
            Method::Apt => "apt-get install",
            Method::Dnf => "dnf install",
            Method::Zypper => "zypper install",
            Method::AppImage => "~/Applications + menu entry",
            Method::Flatpak => "flatpak install",
        }
    }

    pub fn needs_admin(self) -> bool {
        matches!(self, Method::Apt | Method::Dnf | Method::Zypper)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Install(Method),
    /// (headline, what to do instead)
    Refuse(String, String),
}

/// Teku's `teku_route`, plus its "fake package" check from `install_one`.
pub fn route(path: &Path, ty: FileType, fam: OsFamily, flatpak_available: bool) -> Route {
    let refuse = |h: &str, w: &str| Route::Refuse(h.into(), w.into());
    let e = ext(path);
    if ty == FileType::Unknown && matches!(e.as_str(), "deb" | "rpm" | "appimage" | "flatpakref") {
        return Route::Refuse(
            format!("This isn't a real .{e} file"),
            "It may be corrupted, incomplete, or a different file that was renamed. Try downloading it again.".into(),
        );
    }
    match ty {
        FileType::Deb => match fam {
            OsFamily::Debian => Route::Install(Method::Apt),
            _ => refuse("This package is for Debian/Ubuntu systems", "Look for an .rpm, Flatpak or AppImage version of this app instead."),
        },
        FileType::Rpm => match fam {
            OsFamily::Fedora => Route::Install(Method::Dnf),
            OsFamily::Suse => Route::Install(Method::Zypper),
            _ => refuse("This package is for Fedora/openSUSE systems", "Look for a .deb, Flatpak or AppImage version of this app instead."),
        },
        FileType::AppImage => Route::Install(Method::AppImage),
        FileType::FlatpakRef => {
            if flatpak_available {
                Route::Install(Method::Flatpak)
            } else {
                refuse("Flatpak isn't installed", "Install the flatpak package with your package manager, then try again.")
            }
        }
        FileType::Windows => refuse("This is a Windows app", "It can't be installed on Linux. Check the developer's site for a Linux version, or try Wine."),
        FileType::MacOs => refuse("This is a macOS app", "It can't be installed on Linux. Check the developer's site for a Linux version."),
        FileType::Unknown => refuse("Unsupported file type", "Kivo can install .deb, .rpm, .AppImage and .flatpakref files."),
    }
}

/// The command for a system install (everything but AppImages).
pub fn command(method: Method, file: &Path) -> Option<Vec<String>> {
    let f = file.to_string_lossy().into_owned();
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    Some(match method {
        // apt-get treats an absolute path as a local package file.
        Method::Apt => {
            let mut c = v(&["pkexec", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "install", "-y"]);
            c.push(f);
            c
        }
        Method::Dnf => {
            let mut c = v(&["pkexec", "dnf", "install", "-y"]);
            c.push(f);
            c
        }
        // Downloaded RPMs are rarely signed with a key zypper knows; the
        // user already confirmed this exact file.
        Method::Zypper => {
            let mut c = v(&["pkexec", "zypper", "--non-interactive", "install", "--allow-unsigned-rpm"]);
            c.push(f);
            c
        }
        Method::Flatpak => {
            let mut c = v(&["flatpak", "install", "--noninteractive", "--from"]);
            c.push(f);
            c
        }
        Method::AppImage => return None,
    })
}

/// Explain a failed system install. pkexec exits 126 (dismissed / not
/// authorized) or 127 (authentication failed).
pub fn failure(method: Method, code: i32) -> (String, String) {
    if (code == 126 || code == 127) && method.needs_admin() {
        ("Authentication cancelled".into(), "Nothing was installed.".into())
    } else {
        (format!("Install failed (exit {code})"), "See the log for details.".into())
    }
}

/// Run `argv`, streaming each output line (stdout and stderr) to `line`.
/// Returns the exit code (-1 if it couldn't start or was killed).
pub fn run(argv: &[String], log: &mut dyn Write, line: &mut dyn FnMut(&str)) -> i32 {
    let Some((bin, args)) = argv.split_first() else { return -1 };
    let child = Command::new(bin).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("couldn't start {bin}: {e}");
            let _ = writeln!(log, "{msg}");
            line(&msg);
            return -1;
        }
    };
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut readers = Vec::new();
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for l in BufReader::new(stream).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        }));
    }
    drop(tx);
    for l in rx {
        let _ = writeln!(log, "{l}");
        line(&l);
    }
    for r in readers {
        let _ = r.join();
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(-1)
}

// ----------------------------------------------------------------- history

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub when: String,
    /// ok, error, failed, removed
    pub status: String,
    pub kind: String,
    pub file: String,
}

/// Teku's `history_add`: date, status, type, file — tab-separated.
pub fn history_add(state_dir: &Path, status: &str, kind: &str, file: &Path) {
    let _ = std::fs::create_dir_all(state_dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(state_dir.join("history.log")) {
        let when = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
        let _ = writeln!(f, "{when}\t{status}\t{kind}\t{}", file.display());
    }
}

/// Newest first.
pub fn history(state_dir: &Path) -> Vec<HistoryEntry> {
    let text = std::fs::read_to_string(state_dir.join("history.log")).unwrap_or_default();
    let mut v: Vec<HistoryEntry> = text
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(4, '\t');
            Some(HistoryEntry { when: p.next()?.into(), status: p.next()?.into(), kind: p.next()?.into(), file: p.next()?.into() })
        })
        .collect();
    v.reverse();
    v
}

pub fn state_dir() -> PathBuf {
    crate::paths::state_dir().join("install")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, data).unwrap();
        p
    }

    fn deb_bytes() -> Vec<u8> {
        let mut v = b"!<arch>\n".to_vec();
        v.extend_from_slice(b"debian-binary   1700000000  0     0     100644  4         `\n2.0\n");
        v
    }

    #[test]
    fn detects_by_content() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        assert_eq!(file_type(&write(p, "app.deb", &deb_bytes())), FileType::Deb);
        assert_eq!(file_type(&write(p, "renamed.zip", &deb_bytes())), FileType::Deb, "name doesn't matter");
        assert_eq!(file_type(&write(p, "x.rpm", &[0xED, 0xAB, 0xEE, 0xDB, 3, 0])), FileType::Rpm);
        let mut elf = vec![0x7F, b'E', b'L', b'F', 2, 1, 1, 0, b'A', b'I', 2];
        elf.resize(64, 0);
        assert_eq!(file_type(&write(p, "Tool.AppImage", &elf)), FileType::AppImage);
        assert_eq!(file_type(&write(p, "tool-renamed", &elf)), FileType::AppImage);
        let mut plain_elf = elf.clone();
        plain_elf[8] = 0;
        assert_eq!(file_type(&write(p, "plain.AppImage", &plain_elf)), FileType::Unknown);
        assert_eq!(file_type(&write(p, "a.flatpakref", b"[Flatpak Ref]\nName=org.gnome.Maps\nUrl=https://dl.flathub.org/repo/\n")), FileType::FlatpakRef);
        assert_eq!(file_type(&write(p, "setup.exe", b"MZ\x90\x00rest")), FileType::Windows);
        assert_eq!(file_type(&write(p, "setup.bin", b"MZ\x90\x00rest")), FileType::Windows);
        let ole = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0];
        assert_eq!(file_type(&write(p, "x.msi", &ole)), FileType::Windows);
        assert_eq!(file_type(&write(p, "old.doc", &ole)), FileType::Unknown, "an OLE .doc isn't an installer");
        assert_eq!(file_type(&write(p, "x.pkg", b"xar!\x00\x1c")), FileType::MacOs);
        let mut dmg = vec![0u8; 2048];
        dmg[2048 - 512..2048 - 508].copy_from_slice(b"koly");
        assert_eq!(file_type(&write(p, "x.dmg", &dmg)), FileType::MacOs);
        assert_eq!(file_type(&write(p, "x.exe", b"garbage")), FileType::Windows, "foreign extension fallback");
        assert_eq!(file_type(&write(p, "fake.deb", b"<html>not a package</html>")), FileType::Unknown);
    }

    #[test]
    fn os_release_families() {
        let cases = [
            ("ID=ubuntu\nID_LIKE=debian\n", OsFamily::Debian),
            ("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n", OsFamily::Debian),
            ("ID=fedora\n", OsFamily::Fedora),
            ("ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n", OsFamily::Fedora),
            ("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n", OsFamily::Suse),
            ("ID=sles\n", OsFamily::Suse),
            ("ID=arch\n", OsFamily::Arch),
            ("ID=endeavouros\nID_LIKE=arch\n", OsFamily::Arch),
            ("ID=nixos\n", OsFamily::Unknown),
        ];
        for (text, fam) in cases {
            assert_eq!(family(&parse_os_release(text)), fam, "{text}");
        }
        let r = parse_os_release("PRETTY_NAME=\"Ubuntu 26.04 LTS\"\n# comment\n");
        assert_eq!(os_name(&r), "Ubuntu 26.04 LTS");
        assert_eq!(os_name(&HashMap::new()), "Unknown Linux");
    }

    #[test]
    fn routes_like_teku() {
        let p = Path::new("/x/app.deb");
        assert_eq!(route(p, FileType::Deb, OsFamily::Debian, true), Route::Install(Method::Apt));
        assert!(matches!(route(p, FileType::Deb, OsFamily::Fedora, true), Route::Refuse(h, _) if h.contains("Debian/Ubuntu")));
        let r = Path::new("/x/app.rpm");
        assert_eq!(route(r, FileType::Rpm, OsFamily::Fedora, true), Route::Install(Method::Dnf));
        assert_eq!(route(r, FileType::Rpm, OsFamily::Suse, true), Route::Install(Method::Zypper));
        assert!(matches!(route(r, FileType::Rpm, OsFamily::Arch, true), Route::Refuse(h, _) if h.contains("Fedora/openSUSE")));
        assert_eq!(route(Path::new("/x/a.AppImage"), FileType::AppImage, OsFamily::Unknown, false), Route::Install(Method::AppImage));
        let f = Path::new("/x/a.flatpakref");
        assert_eq!(route(f, FileType::FlatpakRef, OsFamily::Arch, true), Route::Install(Method::Flatpak));
        assert!(matches!(route(f, FileType::FlatpakRef, OsFamily::Arch, false), Route::Refuse(h, _) if h.contains("Flatpak isn't installed")));
        assert!(matches!(route(Path::new("/x/s.exe"), FileType::Windows, OsFamily::Debian, true), Route::Refuse(h, w) if h == "This is a Windows app" && w.contains("Wine")));
        assert!(matches!(route(Path::new("/x/s.dmg"), FileType::MacOs, OsFamily::Debian, true), Route::Refuse(h, _) if h == "This is a macOS app"));
        assert!(matches!(route(Path::new("/x/fake.deb"), FileType::Unknown, OsFamily::Debian, true), Route::Refuse(h, w) if h == "This isn't a real .deb file" && w.contains("downloading it again")));
    }

    #[test]
    fn commands_match_teku() {
        let f = Path::new("/home/u/Downloads/app.deb");
        assert_eq!(command(Method::Apt, f).unwrap().join(" "), "pkexec env DEBIAN_FRONTEND=noninteractive apt-get install -y /home/u/Downloads/app.deb");
        assert_eq!(command(Method::Dnf, f).unwrap().join(" "), "pkexec dnf install -y /home/u/Downloads/app.deb");
        assert_eq!(command(Method::Zypper, f).unwrap().join(" "), "pkexec zypper --non-interactive install --allow-unsigned-rpm /home/u/Downloads/app.deb");
        assert_eq!(command(Method::Flatpak, f).unwrap().join(" "), "flatpak install --noninteractive --from /home/u/Downloads/app.deb");
        assert!(command(Method::AppImage, f).is_none());
        assert_eq!(failure(Method::Apt, 126).0, "Authentication cancelled");
        assert_eq!(failure(Method::Apt, 127).0, "Authentication cancelled");
        assert_eq!(failure(Method::Flatpak, 126).0, "Install failed (exit 126)");
        assert_eq!(failure(Method::Dnf, 1).1, "See the log for details.");
    }

    #[test]
    fn runs_and_streams_output() {
        let mut log = Vec::new();
        let mut lines = Vec::new();
        let code = run(&["sh".into(), "-c".into(), "echo one; echo two >&2; exit 3".into()], &mut log, &mut |l| lines.push(l.to_string()));
        assert_eq!(code, 3);
        lines.sort();
        assert_eq!(lines, ["one", "two"]);
        assert!(String::from_utf8(log).unwrap().contains("one"));
        assert_eq!(run(&["/definitely/not/here".into()], &mut Vec::new(), &mut |_| {}), -1);
    }

    #[test]
    fn history_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        history_add(d.path(), "ok", "deb", Path::new("/a/one.deb"));
        history_add(d.path(), "failed", "rpm", Path::new("/a/two.rpm"));
        let h = history(d.path());
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].status.as_str(), h[0].kind.as_str(), h[0].file.as_str()), ("failed", "rpm", "/a/two.rpm"));
        let text = fs::read_to_string(d.path().join("history.log")).unwrap();
        assert_eq!(text.lines().next().unwrap().split('\t').count(), 4);
    }
}
