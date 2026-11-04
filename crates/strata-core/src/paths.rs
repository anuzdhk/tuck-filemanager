// SPDX-License-Identifier: GPL-3.0-or-later
//! XDG directories, user folders and path display helpers.

use std::collections::HashMap;
use std::env;
use std::path::{Component, Path, PathBuf};

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match env::var_os(var) {
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("strata")
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("strata")
}

pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn cache_home() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

/// The well-known user folders that get special icons and sidebar entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserDir {
    Home,
    Desktop,
    Documents,
    Downloads,
    Music,
    Pictures,
    Videos,
    Templates,
    Public,
}

impl UserDir {
    pub const SIDEBAR: [UserDir; 7] = [
        UserDir::Home,
        UserDir::Desktop,
        UserDir::Documents,
        UserDir::Downloads,
        UserDir::Pictures,
        UserDir::Music,
        UserDir::Videos,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UserDir::Home => "Home",
            UserDir::Desktop => "Desktop",
            UserDir::Documents => "Documents",
            UserDir::Downloads => "Downloads",
            UserDir::Music => "Music",
            UserDir::Pictures => "Pictures",
            UserDir::Videos => "Videos",
            UserDir::Templates => "Templates",
            UserDir::Public => "Public",
        }
    }

    fn xdg_key(self) -> Option<&'static str> {
        Some(match self {
            UserDir::Home => return None,
            UserDir::Desktop => "XDG_DESKTOP_DIR",
            UserDir::Documents => "XDG_DOCUMENTS_DIR",
            UserDir::Downloads => "XDG_DOWNLOAD_DIR",
            UserDir::Music => "XDG_MUSIC_DIR",
            UserDir::Pictures => "XDG_PICTURES_DIR",
            UserDir::Videos => "XDG_VIDEOS_DIR",
            UserDir::Templates => "XDG_TEMPLATES_DIR",
            UserDir::Public => "XDG_PUBLICSHARE_DIR",
        })
    }
}

/// Resolved user folders. Reads `user-dirs.dirs`; unset entries fall back to
/// the English defaults only if that folder exists.
#[derive(Debug, Clone)]
pub struct UserDirs {
    map: HashMap<UserDir, PathBuf>,
}

impl UserDirs {
    pub fn load() -> Self {
        let home = home();
        let file = xdg("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs");
        let text = std::fs::read_to_string(file).unwrap_or_default();
        Self::parse(&text, &home)
    }

    pub fn parse(text: &str, home: &Path) -> Self {
        let mut raw = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"');
                let path = if let Some(rest) = v.strip_prefix("$HOME") {
                    home.join(rest.trim_start_matches('/'))
                } else {
                    PathBuf::from(v)
                };
                raw.insert(k.trim().to_string(), path);
            }
        }
        let mut map = HashMap::new();
        map.insert(UserDir::Home, home.to_path_buf());
        for dir in [
            UserDir::Desktop,
            UserDir::Documents,
            UserDir::Downloads,
            UserDir::Music,
            UserDir::Pictures,
            UserDir::Videos,
            UserDir::Templates,
            UserDir::Public,
        ] {
            let key = dir.xdg_key().expect("non-home dirs have keys");
            match raw.get(key) {
                // XDG says a dir equal to $HOME means "disabled".
                Some(p) if p != home => {
                    map.insert(dir, p.clone());
                }
                Some(_) => {}
                None => {
                    let p = home.join(dir.label());
                    if p.is_dir() {
                        map.insert(dir, p);
                    }
                }
            }
        }
        Self { map }
    }

    pub fn get(&self, dir: UserDir) -> Option<&Path> {
        self.map.get(&dir).map(PathBuf::as_path)
    }

    /// Which user folder, if any, `path` is.
    pub fn identify(&self, path: &Path) -> Option<UserDir> {
        self.map.iter().find(|(_, p)| p.as_path() == path).map(|(d, _)| *d)
    }
}

/// One clickable segment of the breadcrumb bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub path: PathBuf,
}

/// Breadcrumb segments. Paths under $HOME start at "Home" like Finder/Dolphin.
pub fn breadcrumbs(path: &Path, home: &Path) -> Vec<Crumb> {
    let mut out = Vec::new();
    let (mut acc, rest) = match path.strip_prefix(home) {
        Ok(rest) if home != Path::new("/") => {
            out.push(Crumb { label: "Home".into(), path: home.to_path_buf() });
            (home.to_path_buf(), rest)
        }
        _ => {
            out.push(Crumb { label: "Computer".into(), path: PathBuf::from("/") });
            (PathBuf::from("/"), path.strip_prefix("/").unwrap_or(path))
        }
    };
    for comp in rest.components() {
        if let Component::Normal(name) = comp {
            acc.push(name);
            out.push(Crumb { label: name.to_string_lossy().into_owned(), path: acc.clone() });
        }
    }
    out
}

/// Path for display: "$HOME/x" becomes "~/x".
pub fn display(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Expand what a user typed into the location bar: "~", "~/x", relative to `cwd`.
pub fn expand_input(input: &str, cwd: &Path, home: &Path) -> PathBuf {
    let input = input.trim();
    if input == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = input.strip_prefix("~/") {
        return home.join(rest);
    }
    if let Some(rest) = input.strip_prefix("file://") {
        return PathBuf::from(rest);
    }
    let p = Path::new(input);
    if p.is_absolute() { p.to_path_buf() } else { normalize(&cwd.join(p)) }
}

/// Lexical normalization: resolves "." and ".." without touching the disk.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("/");
                }
            }
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() { PathBuf::from("/") } else { out }
}

/// Tab completion for the location bar. `typed` is what the user typed,
/// `expanded` its expansion. Returns the new text, or None if nothing matches.
/// A unique folder match gets a trailing slash; several matches complete to
/// their longest common prefix.
pub fn complete(typed: &str, expanded: &Path, show_hidden: bool) -> Option<String> {
    let (dir, prefix) = if typed.ends_with('/') {
        (expanded.to_path_buf(), String::new())
    } else {
        let parent = expanded.parent()?.to_path_buf();
        let name = expanded.file_name()?.to_string_lossy().into_owned();
        (parent, name)
    };
    let mut matches: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false) || e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix))
        .filter(|n| show_hidden || !n.starts_with('.') || prefix.starts_with('.'))
        .collect();
    if matches.is_empty() {
        return None;
    }
    matches.sort();
    let common = matches.iter().skip(1).fold(matches[0].clone(), |acc, m| {
        acc.chars().zip(m.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect()
    });
    let base = &typed[..typed.len() - prefix.len()];
    let mut out = format!("{base}{common}");
    if matches.len() == 1 {
        out.push('/');
    }
    (out != typed).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_dirs() {
        let home = Path::new("/home/u");
        let text = r#"
# comment
XDG_DOCUMENTS_DIR="$HOME/Docs"
XDG_DOWNLOAD_DIR="/data/dl"
XDG_PUBLICSHARE_DIR="$HOME/"
"#;
        let d = UserDirs::parse(text, home);
        assert_eq!(d.get(UserDir::Documents), Some(Path::new("/home/u/Docs")));
        assert_eq!(d.get(UserDir::Downloads), Some(Path::new("/data/dl")));
        assert_eq!(d.get(UserDir::Public), None, "dir equal to $HOME is disabled");
        assert_eq!(d.get(UserDir::Home), Some(home));
        assert_eq!(d.identify(Path::new("/home/u/Docs")), Some(UserDir::Documents));
    }

    #[test]
    fn crumbs_under_home() {
        let c = breadcrumbs(Path::new("/home/u/a/b"), Path::new("/home/u"));
        let labels: Vec<_> = c.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Home", "a", "b"]);
        assert_eq!(c[2].path, Path::new("/home/u/a/b"));
    }

    #[test]
    fn crumbs_outside_home() {
        let c = breadcrumbs(Path::new("/usr/share"), Path::new("/home/u"));
        let labels: Vec<_> = c.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Computer", "usr", "share"]);
        let root = breadcrumbs(Path::new("/"), Path::new("/home/u"));
        assert_eq!(root.len(), 1);
    }

    #[test]
    fn completes_folders() {
        let d = tempfile::tempdir().unwrap();
        for n in ["Documents", "Downloads", "Desktop", ".hidden"] {
            std::fs::create_dir(d.path().join(n)).unwrap();
        }
        std::fs::write(d.path().join("Dfile"), b"").unwrap();
        let base = d.path().to_string_lossy().into_owned();
        let t = format!("{base}/Doc");
        assert_eq!(complete(&t, Path::new(&t), false), Some(format!("{base}/Documents/")));
        let t = format!("{base}/Do");
        assert_eq!(complete(&t, Path::new(&t), false), None, "Do- is already the common prefix");
        let t = format!("{base}/D");
        assert_eq!(complete(&t, Path::new(&t), false), None);
        let t = format!("{base}/.h");
        assert_eq!(complete(&t, Path::new(&t), false), Some(format!("{base}/.hidden/")));
        let t = format!("{base}/x");
        assert_eq!(complete(&t, Path::new(&t), false), None);
    }

    #[test]
    fn display_and_expand() {
        let home = Path::new("/home/u");
        assert_eq!(display(Path::new("/home/u"), home), "~");
        assert_eq!(display(Path::new("/home/u/x"), home), "~/x");
        assert_eq!(display(Path::new("/home/user2"), home), "/home/user2");
        assert_eq!(expand_input("~/x", Path::new("/"), home), Path::new("/home/u/x"));
        assert_eq!(expand_input("../b", Path::new("/a/c"), home), Path::new("/a/b"));
        assert_eq!(expand_input("file:///etc", Path::new("/"), home), Path::new("/etc"));
        assert_eq!(normalize(Path::new("/../..")), Path::new("/"));
    }
}
