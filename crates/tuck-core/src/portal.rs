// SPDX-License-Identifier: GPL-3.0-or-later
//! Tuck as the desktop's file picker: the xdg-desktop-portal FileChooser
//! backend is served by `tuck --portal` (see the app's `portal` module);
//! this part decides where the portal's config lives and edits it.
//!
//! xdg-desktop-portal reads ONE config file: the first of
//! `<desktop>-portals.conf` (for each name in `XDG_CURRENT_DESKTOP`), then
//! `portals.conf`, in the user's config folder, then the system folders.
//! So to pick Tuck we copy the file in effect into the user's folder with a
//! `org.freedesktop.impl.portal.FileChooser=tuck` line added.

use std::path::{Path, PathBuf};

/// The backend name: `tuck.portal`, D-Bus name `…desktop.tuck`.
pub const NAME: &str = "tuck";
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.tuck";
const KEY: &str = "org.freedesktop.impl.portal.FileChooser";
/// First line of a file Tuck created, so turning it off can delete it.
const MARK: &str = "# Made by Tuck to use it as the file picker. Delete this file to undo.";

/// `XDG_CURRENT_DESKTOP` as xdg-desktop-portal reads it: lower case, in order.
pub fn desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().split(':').filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect()
}

/// Folders searched for the config, user's first.
pub fn config_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![crate::paths::config_home().join("xdg-desktop-portal")];
    let conf = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    dirs.extend(std::env::split_paths(&conf).map(|d| d.join("xdg-desktop-portal")));
    dirs.push("/etc/xdg-desktop-portal".into());
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(std::env::split_paths(&data).map(|d| d.join("xdg-desktop-portal")));
    dirs
}

/// Is the backend installed (a `tuck.portal` the portal can find)?
pub fn installed() -> bool {
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    std::env::split_paths(&data).any(|d| d.join("xdg-desktop-portal/portals/tuck.portal").is_file())
}

/// The config file in effect.
pub fn current_config(dirs: &[PathBuf], desktops: &[String]) -> Option<PathBuf> {
    dirs.iter().find_map(|d| {
        desktops.iter().map(|n| d.join(format!("{n}-portals.conf"))).chain([d.join("portals.conf")]).find(|p| p.is_file())
    })
}

/// The file Tuck writes: the user's, for the first desktop name.
pub fn user_config(dirs: &[PathBuf], desktops: &[String]) -> PathBuf {
    let name = desktops.first().map(|d| format!("{d}-portals.conf")).unwrap_or_else(|| "portals.conf".into());
    dirs[0].join(name)
}

/// `text` with the FileChooser line set to `backend` (None removes it).
pub fn with_file_chooser(text: &str, backend: Option<&str>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_preferred = false;
    let mut seen_preferred = false;
    let mut done = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if in_preferred && !done {
                if let Some(b) = backend {
                    out.push(format!("{KEY}={b};"));
                }
                done = true;
            }
            in_preferred = t == "[preferred]";
            seen_preferred |= in_preferred;
        } else if in_preferred && t.split('=').next().map(str::trim) == Some(KEY) {
            if let (Some(b), false) = (backend, done) {
                out.push(format!("{KEY}={b};"));
            }
            done = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !done && let Some(b) = backend {
        if !seen_preferred {
            out.push("[preferred]".into());
        }
        out.push(format!("{KEY}={b};"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Is Tuck the picker in this config text?
pub fn uses_tuck(text: &str) -> bool {
    let mut in_preferred = false;
    text.lines().any(|l| {
        let t = l.trim();
        if t.starts_with('[') {
            in_preferred = t == "[preferred]";
            return false;
        }
        in_preferred && t.split_once('=').is_some_and(|(k, v)| k.trim() == KEY && v.split(';').next().map(str::trim) == Some(NAME))
    })
}

pub fn is_enabled() -> bool {
    let (dirs, desk) = (config_dirs(), desktops());
    current_config(&dirs, &desk).and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|t| uses_tuck(&t))
}

/// Make Tuck the file picker, or stop. Returns the file written (or removed).
pub fn set_enabled(on: bool) -> std::io::Result<PathBuf> {
    set_enabled_in(&config_dirs(), &desktops(), on)
}

pub fn set_enabled_in(dirs: &[PathBuf], desktops: &[String], on: bool) -> std::io::Result<PathBuf> {
    let target = user_config(dirs, desktops);
    let base = current_config(dirs, desktops);
    let text = base.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    if on {
        // Keep everything the desktop set; only the picker changes.
        let mut new = with_file_chooser(&text, Some(NAME));
        if base.as_deref() != Some(target.as_path()) {
            new = format!("{MARK}\n{new}");
        }
        std::fs::create_dir_all(target.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(&target, new)?;
    } else if let Some(p) = base.filter(|p| p.starts_with(&dirs[0])) {
        if text.starts_with(MARK) {
            std::fs::remove_file(&p)?;
        } else {
            std::fs::write(&p, with_file_chooser(&text, None))?;
        }
        return Ok(p);
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_the_preferred_section() {
        let gnome = "[preferred]\ndefault=gnome;gtk;\norg.freedesktop.impl.portal.Secret=gnome-keyring;\n";
        let on = with_file_chooser(gnome, Some("tuck"));
        assert_eq!(on, "[preferred]\ndefault=gnome;gtk;\norg.freedesktop.impl.portal.Secret=gnome-keyring;\norg.freedesktop.impl.portal.FileChooser=tuck;\n");
        assert!(uses_tuck(&on) && !uses_tuck(gnome));
        // Replaces an existing choice, and removing restores the original.
        let kde = with_file_chooser(&on.replace("tuck;", "kde;"), Some("tuck"));
        assert_eq!(kde.matches("FileChooser").count(), 1);
        assert_eq!(with_file_chooser(&on, None), gnome);
        // Another section after [preferred]: the line goes before it.
        let two = with_file_chooser("[preferred]\ndefault=gtk;\n[other]\nx=1\n", Some("tuck"));
        assert_eq!(two, "[preferred]\ndefault=gtk;\norg.freedesktop.impl.portal.FileChooser=tuck;\n[other]\nx=1\n");
        assert_eq!(with_file_chooser("", Some("tuck")), "[preferred]\norg.freedesktop.impl.portal.FileChooser=tuck;\n");
    }

    #[test]
    fn turns_on_and_off_without_losing_the_desktops_choices() {
        let d = tempfile::tempdir().unwrap();
        let (user, sys) = (d.path().join("user"), d.path().join("sys"));
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("gnome-portals.conf"), "[preferred]\ndefault=gnome;gtk;\n").unwrap();
        let dirs = vec![user.clone(), sys.clone()];
        let desk = vec!["ubuntu".to_string(), "gnome".to_string()];
        assert_eq!(current_config(&dirs, &desk), Some(sys.join("gnome-portals.conf")));
        let written = set_enabled_in(&dirs, &desk, true).unwrap();
        assert_eq!(written, user.join("ubuntu-portals.conf"));
        let text = std::fs::read_to_string(&written).unwrap();
        assert!(text.contains("default=gnome;gtk;") && uses_tuck(&text));
        assert_eq!(current_config(&dirs, &desk), Some(written.clone()));
        // Off: our own file goes away; the system file is untouched.
        set_enabled_in(&dirs, &desk, false).unwrap();
        assert!(!written.exists());
        assert_eq!(std::fs::read_to_string(sys.join("gnome-portals.conf")).unwrap(), "[preferred]\ndefault=gnome;gtk;\n");
        // A file the user wrote themselves only loses our line.
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("portals.conf"), "[preferred]\ndefault=kde;\n").unwrap();
        set_enabled_in(&dirs, &desk, true).unwrap();
        set_enabled_in(&dirs, &desk, false).unwrap();
        assert_eq!(std::fs::read_to_string(user.join("portals.conf")).unwrap(), "[preferred]\ndefault=kde;\n");
    }
}
