// SPDX-License-Identifier: GPL-3.0-or-later
//! AppImages, ported from Teku's install_appimage.sh: they live in
//! ~/Applications with a menu entry and icon. Entries are named
//! `teku-<id>.desktop` and marked `X-Teku-AppImage=<path>`, exactly like
//! Teku's, so Kivo and Teku can each list and remove what the other made.
//!
//! Two deliberate differences, both to never lose data: an existing
//! AppImage with the same name is moved to the trash instead of being
//! overwritten, and removal trashes the AppImage (Teku falls back to `rm`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::ops::{self, OpError};
use crate::trash::Trash;

#[derive(Debug, Clone)]
pub struct Dirs {
    pub apps: PathBuf,
    pub menu: PathBuf,
    pub icons: PathBuf,
}

impl Dirs {
    /// Teku's locations (`TEKU_APPS_DIR` is honoured, as in Teku).
    pub fn user() -> Dirs {
        // KIVO_APPIMAGE_DATA redirects the menu and icon folders (tests only).
        let data = std::env::var_os("KIVO_APPIMAGE_DATA").map(PathBuf::from).unwrap_or_else(crate::paths::data_home);
        let apps = std::env::var_os("TEKU_APPS_DIR").map(PathBuf::from).unwrap_or_else(|| crate::paths::home().join("Applications"));
        Dirs { apps, menu: data.join("applications"), icons: data.join("icons").join("teku") }
    }
}

/// Menu-safe id from a file name: "Foo App-1.2-x86_64.AppImage" →
/// "foo-app-1.2-x86_64" (Teku's `teku_appimage_id`).
pub fn id(path: &Path) -> String {
    let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let base = match base.rfind('.') {
        Some(i) => base[..i].to_string(),
        None => base,
    };
    base.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') { c } else { '-' })
        .collect()
}

pub fn dest(dirs: &Dirs, src: &Path) -> PathBuf {
    dirs.apps.join(src.file_name().unwrap_or_default())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub dest: PathBuf,
    pub entry: PathBuf,
    pub name: String,
    pub icon: Option<PathBuf>,
}

/// Quote a path for a desktop entry's Exec key.
fn exec_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    // Desktop entry values escape backslashes once more.
    out.replace('\\', "\\\\")
}

fn one_line(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
}

/// Pull one path out of the AppImage into `dir` (Teku's
/// `_teku_appimage_extract`). Type 1 images can't do this; callers fall back.
fn extract(app: &Path, dir: &Path, pattern: &str) -> bool {
    let child = Command::new(app)
        .arg("--appimage-extract")
        .arg(pattern)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if start.elapsed() > Duration::from_secs(30) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return false,
        }
    }
}

/// Install `src`. `log` receives progress lines, as Teku printed them.
pub fn install(dirs: &Dirs, src: &Path, trash: &Trash, log: &mut dyn FnMut(&str)) -> Result<Installed, OpError> {
    let src = fs::canonicalize(src).map_err(|e| OpError::io(&e, src, "install"))?;
    let dest = dest(dirs, &src);
    let id = id(&src);
    for d in [&dirs.apps, &dirs.menu, &dirs.icons] {
        fs::create_dir_all(d).map_err(|e| OpError::io(&e, d, "create"))?;
    }
    if src != dest {
        if fs::symlink_metadata(&dest).is_ok() {
            log(&format!("moving the older {} to the trash", dest.file_name().unwrap_or_default().to_string_lossy()));
            trash.trash(&dest)?;
        }
        log(&format!("moving to {}", dest.display()));
        match ops::rename_noreplace(&src, &dest) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                let r = ops::transfer(ops::Mode::Move, std::slice::from_ref(&src), &dirs.apps, &mut ops::Fixed(ops::Resolution::Skip), &ops::Cancel::new());
                if let Some(e) = r.errors.into_iter().next() {
                    return Err(e);
                }
            }
            Err(e) => return Err(OpError::io(&e, &src, "move")),
        }
    }
    let mode = fs::metadata(&dest).map_err(|e| OpError::io(&e, &dest, "read"))?.permissions().mode();
    fs::set_permissions(&dest, fs::Permissions::from_mode(mode | 0o755)).map_err(|e| OpError::io(&e, &dest, "make executable"))?;

    log("reading app metadata");
    let mut name = String::new();
    let mut icon = None;
    let tmp = std::env::temp_dir().join(format!("kivo-appimage-{}-{}", std::process::id(), id));
    let _ = fs::remove_dir_all(&tmp);
    if fs::create_dir_all(&tmp).is_ok() {
        let root = tmp.join("squashfs-root");
        if extract(&dest, &tmp, "*.desktop") {
            let desktop = fs::read_dir(&root)
                .ok()
                .and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "desktop")));
            if let Some(text) = desktop.and_then(|d| fs::read_to_string(d).ok()) {
                name = text.lines().find_map(|l| l.strip_prefix("Name=")).map(one_line).unwrap_or_default();
            }
            // .DirIcon is usually a symlink into the image; extract its target too.
            if extract(&dest, &tmp, ".DirIcon") {
                let di = root.join(".DirIcon");
                if let Ok(target) = fs::read_link(&di) {
                    extract(&dest, &tmp, &target.to_string_lossy());
                }
                // Follow the link, but only inside the extracted tree.
                let real = fs::canonicalize(&di).ok().filter(|r| r.starts_with(&tmp));
                if let Some(real) = real.filter(|r| fs::metadata(r).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)) {
                    let png = fs::read(&real).map(|b| b.starts_with(b"\x89PNG")).unwrap_or(false);
                    let target = dirs.icons.join(format!("{id}.{}", if png { "png" } else { "svg" }));
                    if fs::copy(&real, &target).is_ok() {
                        icon = Some(target);
                    }
                }
            }
        }
        let _ = fs::remove_dir_all(&tmp);
    }

    log("creating menu entry");
    let entry = dirs.menu.join(format!("teku-{id}.desktop"));
    let shown = if name.is_empty() { id.clone() } else { name.clone() };
    let icon_value = icon.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "application-x-executable".into());
    let text = format!(
        "[Desktop Entry]\nType=Application\nName={shown}\nExec={} %U\nIcon={icon_value}\nTerminal=false\nCategories=Utility;\nX-Teku-AppImage={}\n",
        exec_quote(&dest),
        dest.display()
    );
    fs::write(&entry, text).map_err(|e| OpError::io(&e, &entry, "create"))?;
    refresh_menu(&dirs.menu);
    log("done");
    Ok(Installed { dest, entry, name: shown, icon })
}

fn refresh_menu(menu: &Path) {
    if crate::terminal::on_path("update-desktop-database") {
        let _ = Command::new("update-desktop-database").arg(menu).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

/// AppImages installed by Kivo or Teku (Teku's `teku_appimage_list`).
pub fn list(dirs: &Dirs) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&dirs.menu) else { return out };
    let mut entries: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("teku-") && n.ends_with(".desktop")))
        .collect();
    entries.sort();
    for e in entries {
        let Ok(text) = fs::read_to_string(&e) else { continue };
        let app = text.lines().find_map(|l| l.strip_prefix("X-Teku-AppImage="));
        let name = text.lines().find_map(|l| l.strip_prefix("Name=")).unwrap_or("");
        if let Some(a) = app {
            out.push((PathBuf::from(a), name.to_string()));
        }
    }
    out
}

/// Remove an AppImage this tool (or Teku) installed: menu entry, its icon,
/// and the AppImage itself — moved to the trash so it can be restored.
pub fn uninstall(dirs: &Dirs, app: &Path, trash: &Trash) -> Result<(), OpError> {
    let Ok(rd) = fs::read_dir(&dirs.menu) else {
        return Err(not_ours(app));
    };
    let marker = format!("X-Teku-AppImage={}", app.display());
    let entry = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("teku-") && n.ends_with(".desktop")))
        .find(|p| fs::read_to_string(p).is_ok_and(|t| t.lines().any(|l| l == marker)));
    let Some(entry) = entry else { return Err(not_ours(app)) };
    if fs::symlink_metadata(app).is_ok() {
        trash.trash(app)?;
    }
    let text = fs::read_to_string(&entry).unwrap_or_default();
    if let Some(icon) = text.lines().find_map(|l| l.strip_prefix("Icon=")).map(PathBuf::from)
        && icon.starts_with(&dirs.icons) {
            let _ = fs::remove_file(icon);
        }
    fs::remove_file(&entry).map_err(|e| OpError::io(&e, &entry, "remove"))?;
    refresh_menu(&dirs.menu);
    Ok(())
}

fn not_ours(app: &Path) -> OpError {
    OpError::new(app, "This AppImage wasn't installed by Kivo or Teku.", "Only AppImages installed with them can be removed here.")
}

/// Is `path` an AppImage that was installed through Kivo/Teku?
pub fn is_installed(dirs: &Dirs, path: &Path) -> bool {
    list(dirs).iter().any(|(p, _)| p == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(root: &Path) -> Dirs {
        Dirs { apps: root.join("Applications"), menu: root.join("share/applications"), icons: root.join("share/icons/teku") }
    }

    /// A stand-in AppImage that supports --appimage-extract like a type 2 image.
    fn fake_appimage(path: &Path) {
        let script = r#"#!/bin/sh
[ "$1" = "--appimage-extract" ] || exit 0
mkdir -p squashfs-root
case "$2" in
  '*.desktop') printf '[Desktop Entry]\nType=Application\nName=Fake Tool\n' > squashfs-root/fake.desktop ;;
  .DirIcon) ln -sf usr/share/icon.png squashfs-root/.DirIcon ;;
  usr/share/icon.png) mkdir -p squashfs-root/usr/share; printf '\211PNG\r\n\032\nfake' > squashfs-root/usr/share/icon.png ;;
esac
"#;
        fs::write(path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn ids_like_teku() {
        assert_eq!(id(Path::new("/d/Foo App-1.2-x86_64.AppImage")), "foo-app-1.2-x86_64");
        assert_eq!(id(Path::new("Ünïcode Thing.AppImage")), "-n-code-thing");
    }

    #[test]
    fn installs_lists_and_uninstalls() {
        let d = tempfile::tempdir().unwrap();
        let dl = d.path().join("Downloads");
        fs::create_dir_all(&dl).unwrap();
        let src = dl.join("Fake Tool-1.0.AppImage");
        fake_appimage(&src);
        let dirs = dirs(d.path());
        let trash = Trash::with_home(d.path().join("Trash"));
        let mut lines = Vec::new();
        let done = install(&dirs, &src, &trash, &mut |l| lines.push(l.to_string())).unwrap();
        assert!(!src.exists(), "moved, not copied");
        assert_eq!(done.dest, dirs.apps.join("Fake Tool-1.0.AppImage"));
        assert!(fs::metadata(&done.dest).unwrap().permissions().mode() & 0o111 != 0, "executable");
        assert_eq!(done.name, "Fake Tool");
        let icon = done.icon.clone().expect("icon extracted");
        assert_eq!(icon, dirs.icons.join("fake-tool-1.0.png"));
        let entry = fs::read_to_string(&done.entry).unwrap();
        assert!(entry.contains("Name=Fake Tool\n"));
        assert!(entry.contains(&format!("X-Teku-AppImage={}\n", done.dest.display())));
        assert!(entry.contains("Exec=\"") && entry.contains(" %U\n"));
        assert_eq!(done.entry.file_name().unwrap(), "teku-fake-tool-1.0.desktop");
        assert_eq!(lines.first().unwrap(), &format!("moving to {}", done.dest.display()));
        assert_eq!(lines.last().unwrap(), "done");
        assert_eq!(list(&dirs), vec![(done.dest.clone(), "Fake Tool".to_string())]);

        // Installing a newer copy keeps the old one in the trash.
        fake_appimage(&src);
        install(&dirs, &src, &trash, &mut |_| {}).unwrap();
        assert_eq!(trash.list().len(), 1);

        uninstall(&dirs, &done.dest, &trash).unwrap();
        assert!(!done.dest.exists() && !done.entry.exists() && !icon.exists());
        assert_eq!(trash.list().len(), 2, "removed AppImage is recoverable");
        assert!(list(&dirs).is_empty());
        let e = uninstall(&dirs, &done.dest, &trash).unwrap_err();
        assert!(e.message.contains("wasn't installed by Kivo or Teku"));
    }

    #[test]
    fn falls_back_without_metadata() {
        // A type 1 image (or anything that can't self-extract) still gets an entry.
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("Old.AppImage");
        fs::write(&src, b"\x7fELF not really").unwrap();
        let dirs = dirs(d.path());
        let done = install(&dirs, &src, &Trash::with_home(d.path().join("Trash")), &mut |_| {}).unwrap();
        assert_eq!(done.name, "old");
        assert!(done.icon.is_none());
        assert!(fs::read_to_string(&done.entry).unwrap().contains("Icon=application-x-executable"));
    }

    #[test]
    fn exec_paths_are_escaped() {
        assert_eq!(exec_quote(Path::new("/a b/x\"$y.AppImage")), r#""/a b/x\\"\\$y.AppImage""#);
    }
}
