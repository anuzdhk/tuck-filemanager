// SPDX-License-Identifier: GPL-3.0-or-later
//! Dolphin-compatible service menus: extra context-menu actions installed as
//! `.desktop` files in `kio/servicemenus` (and the older
//! `kservices5/ServiceMenus`). Tuck reads the same files Dolphin does, so
//! actions installed for Dolphin (or from the KDE Store) show up here too.
//!
//! Like Dolphin, files in the user's own folder must be executable before
//! they are trusted; system folders are trusted as installed.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceAction {
    pub name: String,
    pub icon: Option<String>,
    pub exec: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceMenu {
    pub file: PathBuf,
    /// MIME types (wildcards like `image/*`, and `all/all`, `all/allfiles`).
    pub mimes: Vec<String>,
    pub excluded: Vec<String>,
    pub min_urls: usize,
    pub max_urls: Option<usize>,
    /// URL schemes the menu accepts; empty = any.
    pub protocols: Vec<String>,
    /// Put the actions in a submenu with this name.
    pub submenu: Option<String>,
    pub actions: Vec<ServiceAction>,
}

/// What a service menu needs to know about one selected item.
pub struct Picked<'a> {
    pub mime: &'a str,
    pub is_dir: bool,
    pub scheme: &'a str,
}

/// Parse a desktop file into groups of key → value (untranslated keys only).
pub fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut groups: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut cur = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            cur = line[1..line.len() - 1].to_string();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if k.contains('[') {
                continue;
            }
            groups.entry(cur.clone()).or_default().insert(k.to_string(), v.trim().to_string());
        }
    }
    groups
}

fn list(v: Option<&String>) -> Vec<String> {
    v.map(|s| s.split([';', ',']).map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default()
}

pub fn parse(text: &str, file: &Path, on_path: &dyn Fn(&str) -> bool) -> Option<ServiceMenu> {
    let g = parse_ini(text);
    let entry = g.get("Desktop Entry")?;
    if entry.get("Type").is_some_and(|t| t != "Service") {
        return None;
    }
    if entry.get("Hidden").is_some_and(|h| h == "true") {
        return None;
    }
    if let Some(t) = entry.get("TryExec")
        && !on_path(t)
    {
        return None;
    }
    let mut mimes = list(entry.get("MimeType"));
    // Old style: MIME types listed among the service types.
    for st in list(entry.get("X-KDE-ServiceTypes")).into_iter().chain(list(entry.get("ServiceTypes"))) {
        if !st.starts_with("KonqPopupMenu") && st.contains('/') {
            mimes.push(st);
        }
    }
    let excluded = list(entry.get("ExcludeMimeTypes"));
    let mut actions = Vec::new();
    for id in list(entry.get("Actions")) {
        let Some(a) = g.get(&format!("Desktop Action {id}")) else { continue };
        let (Some(name), Some(exec)) = (a.get("Name"), a.get("Exec")) else { continue };
        if let Some(t) = a.get("TryExec")
            && !on_path(t)
        {
            continue;
        }
        actions.push(ServiceAction { name: name.clone(), icon: a.get("Icon").cloned(), exec: exec.clone() });
    }
    if actions.is_empty() {
        return None;
    }
    Some(ServiceMenu {
        file: file.to_path_buf(),
        mimes,
        excluded,
        min_urls: entry.get("X-KDE-MinNumberOfUrls").and_then(|v| v.parse().ok()).unwrap_or(0),
        max_urls: entry.get("X-KDE-MaxNumberOfUrls").and_then(|v| v.parse().ok()),
        protocols: list(entry.get("X-KDE-Protocols")),
        submenu: entry.get("X-KDE-Submenu").filter(|s| !s.is_empty()).cloned(),
        actions,
    })
}

/// Does `mime` match `pattern`? `is_a` asks the system MIME database about
/// subclasses (text/x-rust is a text/plain).
fn mime_matches(pattern: &str, p: &Picked, is_a: &dyn Fn(&str, &str) -> bool) -> bool {
    match pattern {
        "all/all" | "*" | "*/*" => true,
        "all/allfiles" => !p.is_dir,
        "application/octet-stream" => !p.is_dir,
        pat if pat.ends_with("/*") => p.mime.starts_with(&pat[..pat.len() - 1]),
        pat => p.mime == pat || is_a(p.mime, pat),
    }
}

impl ServiceMenu {
    /// Does this menu apply to the whole selection?
    pub fn applies(&self, picked: &[Picked], is_a: &dyn Fn(&str, &str) -> bool) -> bool {
        let n = picked.len();
        if n == 0 || n < self.min_urls || self.max_urls.is_some_and(|m| n > m) {
            return false;
        }
        picked.iter().all(|p| {
            (self.protocols.is_empty() || self.protocols.iter().any(|s| s == p.scheme))
                && self.mimes.iter().any(|m| mime_matches(m, p, is_a))
                && !self.excluded.iter().any(|m| mime_matches(m, p, is_a))
        })
    }
}

fn dirs() -> Vec<(PathBuf, bool)> {
    let mut out = vec![
        (crate::paths::data_home().join("kio/servicemenus"), true),
        (crate::paths::data_home().join("kservices5/ServiceMenus"), true),
    ];
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for d in sys.split(':').filter(|d| !d.is_empty()) {
        out.push((Path::new(d).join("kio/servicemenus"), false));
        out.push((Path::new(d).join("kservices5/ServiceMenus"), false));
    }
    out
}

/// Every installed service menu. A user file hides a system one of the
/// same name.
pub fn load(on_path: &dyn Fn(&str) -> bool) -> Vec<ServiceMenu> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (dir, user) in dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "desktop")).collect();
        files.sort();
        for f in files {
            let Some(name) = f.file_name().map(|n| n.to_os_string()) else { continue };
            if !seen.insert(name) {
                continue;
            }
            if user && !std::fs::metadata(&f).is_ok_and(|m| m.permissions().mode() & 0o111 != 0) {
                // Dolphin ignores non-executable user service menus too.
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            if let Some(m) = parse(&text, &f, on_path) {
                out.push(m);
            }
        }
    }
    out
}

/// Build the command lines for `exec` (desktop-entry field codes). `%f`/`%u`
/// take one item, so they give one command per item; `%F`/`%U` take all.
pub fn commands(exec: &str, files: &[PathBuf], uris: &[String], name: &str, icon: Option<&str>) -> Vec<Vec<String>> {
    let args = crate::thumbs::split_exec(exec);
    let single = args.iter().any(|a| a.contains("%f") || a.contains("%u"));
    let runs: Vec<usize> = if single { (0..files.len().max(1)).collect() } else { vec![usize::MAX] };
    let mut out = Vec::new();
    for run in runs {
        let mut argv = Vec::new();
        for a in &args {
            match a.as_str() {
                "%F" => argv.extend(files.iter().map(|f| f.to_string_lossy().into_owned())),
                "%U" => argv.extend(uris.iter().cloned()),
                "%i" => {
                    if let Some(i) = icon {
                        argv.push("--icon".into());
                        argv.push(i.into());
                    }
                }
                _ => {
                    let f = files.get(run).map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    let u = uris.get(run).cloned().unwrap_or_default();
                    let mut s = String::new();
                    let mut chars = a.chars();
                    while let Some(c) = chars.next() {
                        if c != '%' {
                            s.push(c);
                            continue;
                        }
                        match chars.next() {
                            Some('f') => s.push_str(&f),
                            Some('u') => s.push_str(&u),
                            Some('c') => s.push_str(name),
                            Some('%') => s.push('%'),
                            // %k, %d, %D, %n, %N, %v, %m: deprecated or unused.
                            _ => {}
                        }
                    }
                    if !s.is_empty() || !a.starts_with('%') {
                        argv.push(s);
                    }
                }
            }
        }
        if !argv.is_empty() {
            out.push(argv);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KONSOLE: &str = "[Desktop Entry]\nType=Service\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin\nMimeType=application/x-executable;\nActions=run;\n\n[Desktop Action run]\nTryExec=konsole\nExec=konsole --hold -e %f\nIcon=utilities-terminal\nName=Run In Konsole\nName[de]=In Konsole ausführen\n";
    const FONTS: &str = "[Desktop Entry]\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin,font/ttf,font/otf\nActions=install;\nType=Service\nX-KDE-Submenu=Fonts\nX-KDE-MaxNumberOfUrls=3\n\n[Desktop Action install]\nName=Install...\nExec=kfontinst %U\n";

    fn none(_: &str, _: &str) -> bool {
        false
    }

    #[test]
    fn parses_dolphin_menus() {
        let m = parse(KONSOLE, Path::new("k.desktop"), &|_| true).expect("menu");
        assert_eq!(m.mimes, vec!["application/x-executable"]);
        assert_eq!(m.actions[0].name, "Run In Konsole");
        assert_eq!(m.actions[0].icon.as_deref(), Some("utilities-terminal"));
        // Missing program: action (and so the menu) is dropped.
        assert!(parse(KONSOLE, Path::new("k.desktop"), &|_| false).is_none());
        let f = parse(FONTS, Path::new("f.desktop"), &|_| true).expect("menu");
        assert_eq!(f.mimes, vec!["font/ttf", "font/otf"]);
        assert_eq!(f.submenu.as_deref(), Some("Fonts"));
    }

    #[test]
    fn matching() {
        let f = parse(FONTS, Path::new("f.desktop"), &|_| true).expect("menu");
        let ttf = Picked { mime: "font/ttf", is_dir: false, scheme: "file" };
        let png = Picked { mime: "image/png", is_dir: false, scheme: "file" };
        assert!(f.applies(&[ttf], &none));
        assert!(!f.applies(&[Picked { mime: "font/ttf", is_dir: false, scheme: "file" }, png], &none));
        let four: Vec<Picked> = (0..4).map(|_| Picked { mime: "font/ttf", is_dir: false, scheme: "file" }).collect();
        assert!(!f.applies(&four, &none), "max urls");
        let mut all = f.clone();
        all.mimes = vec!["all/allfiles".into()];
        all.protocols = vec!["file".into()];
        assert!(all.applies(&[Picked { mime: "x/y", is_dir: false, scheme: "file" }], &none));
        assert!(!all.applies(&[Picked { mime: "inode/directory", is_dir: true, scheme: "file" }], &none));
        assert!(!all.applies(&[Picked { mime: "x/y", is_dir: false, scheme: "sftp" }], &none));
        let mut img = f.clone();
        img.mimes = vec!["image/*".into()];
        assert!(img.applies(&[Picked { mime: "image/png", is_dir: false, scheme: "file" }], &none));
        let mut text = f;
        text.mimes = vec!["text/plain".into()];
        let rust = Picked { mime: "text/x-rust", is_dir: false, scheme: "file" };
        assert!(text.applies(&[rust], &|a, b| a == "text/x-rust" && b == "text/plain"));
    }

    #[test]
    fn field_codes() {
        let files = vec![PathBuf::from("/a/x y.ttf"), PathBuf::from("/a/z.ttf")];
        let uris = vec!["file:///a/x%20y.ttf".to_string(), "file:///a/z.ttf".to_string()];
        assert_eq!(commands("kfontinst %U", &files, &uris, "Install", None), vec![vec!["kfontinst", "file:///a/x%20y.ttf", "file:///a/z.ttf"]]);
        assert_eq!(
            commands("konsole --hold -e %f", &files, &uris, "Run", None),
            vec![vec!["konsole", "--hold", "-e", "/a/x y.ttf"], vec!["konsole", "--hold", "-e", "/a/z.ttf"]]
        );
        assert_eq!(commands("\"/opt/my app\" --x=%f 100%%", &files[..1], &uris[..1], "n", None), vec![vec!["/opt/my app", "--x=/a/x y.ttf", "100%"]]);
        assert_eq!(commands("tool %F %i %c", &files[..1], &uris[..1], "Name", Some("ic")), vec![vec!["tool", "/a/x y.ttf", "--icon", "ic", "Name"]]);
    }
}
