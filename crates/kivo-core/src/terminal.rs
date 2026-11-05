// SPDX-License-Identifier: GPL-3.0-or-later
//! "Open in Terminal": pick an installed terminal and the flag that sets its
//! working folder. The process's current directory is set too, which covers
//! terminals without such a flag.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
}

/// (binary, how to pass the folder)
const KNOWN: &[(&str, Flag)] = &[
    ("ptyxis", Flag::Eq("--working-directory")),
    ("kgx", Flag::Eq("--working-directory")),
    ("gnome-terminal", Flag::Eq("--working-directory")),
    ("konsole", Flag::Sep("--workdir")),
    ("xfce4-terminal", Flag::Eq("--working-directory")),
    ("mate-terminal", Flag::Eq("--working-directory")),
    ("lxterminal", Flag::Eq("--working-directory")),
    ("qterminal", Flag::Sep("-w")),
    ("tilix", Flag::Eq("--working-directory")),
    ("terminator", Flag::Eq("--working-directory")),
    ("kitty", Flag::Sep("--directory")),
    ("alacritty", Flag::Sep("--working-directory")),
    ("foot", Flag::Eq("--working-directory")),
    ("wezterm", Flag::Wezterm),
    ("ghostty", Flag::Eq("--working-directory")),
    ("x-terminal-emulator", Flag::None),
    ("xterm", Flag::None),
];

#[derive(Debug, Clone, Copy)]
enum Flag {
    Eq(&'static str),
    Sep(&'static str),
    Wezterm,
    None,
}

fn args_for(flag: Flag, dir: &Path) -> Vec<String> {
    let d = dir.to_string_lossy().into_owned();
    match flag {
        Flag::Eq(f) => vec![format!("{f}={d}")],
        Flag::Sep(f) => vec![f.to_string(), d],
        Flag::Wezterm => vec!["start".into(), "--cwd".into(), d],
        Flag::None => vec![],
    }
}

/// Choose a terminal. Order: `$TERMINAL`, the desktop's own terminal, then
/// the list above. `installed` reports whether a binary is on `$PATH`.
pub fn command_for(
    dir: &Path,
    env_terminal: Option<&str>,
    desktop: Option<&str>,
    installed: &dyn Fn(&str) -> bool,
) -> Option<Command> {
    if let Some(t) = env_terminal.map(str::trim).filter(|t| !t.is_empty()) {
        let bin = Path::new(t).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let flag = KNOWN.iter().find(|(b, _)| *b == bin).map(|(_, f)| *f).unwrap_or(Flag::None);
        return Some(Command { program: t.to_string(), args: args_for(flag, dir) });
    }
    let desktop = desktop.unwrap_or("").to_ascii_lowercase();
    let preferred: &[&str] = if desktop.contains("kde") {
        &["konsole"]
    } else if desktop.contains("xfce") {
        &["xfce4-terminal"]
    } else if desktop.contains("mate") {
        &["mate-terminal"]
    } else if desktop.contains("lxqt") {
        &["qterminal"]
    } else if desktop.contains("lxde") {
        &["lxterminal"]
    } else if desktop.contains("gnome") || desktop.contains("unity") {
        &["ptyxis", "kgx", "gnome-terminal"]
    } else {
        &[]
    };
    let order = preferred.iter().copied().chain(KNOWN.iter().map(|(b, _)| *b));
    for bin in order {
        if installed(bin) {
            let flag = KNOWN.iter().find(|(b, _)| *b == bin).map(|(_, f)| *f).unwrap_or(Flag::None);
            return Some(Command { program: bin.to_string(), args: args_for(flag, dir) });
        }
    }
    None
}

/// Is `bin` an executable on `$PATH`?
pub fn on_path(bin: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|d| {
        std::fs::metadata(d.join(bin)).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_env_then_desktop_then_list() {
        let dir = Path::new("/home/u/My Docs");
        let all = |_: &str| true;
        let c = command_for(dir, Some("kitty"), Some("GNOME"), &all).unwrap();
        assert_eq!(c.args, ["--directory", "/home/u/My Docs"]);
        let c = command_for(dir, None, Some("KDE"), &all).unwrap();
        assert_eq!(c.program, "konsole");
        assert_eq!(c.args, ["--workdir", "/home/u/My Docs"]);
        let c = command_for(dir, None, Some("ubuntu:GNOME"), &all).unwrap();
        assert_eq!(c.program, "ptyxis");
        assert_eq!(c.args, ["--working-directory=/home/u/My Docs"]);
        let only_xterm = |b: &str| b == "xterm";
        let c = command_for(dir, None, Some("sway"), &only_xterm).unwrap();
        assert_eq!((c.program.as_str(), c.args.len()), ("xterm", 0));
        assert!(command_for(dir, None, None, &|_| false).is_none());
        let c = command_for(dir, None, None, &|b| b == "wezterm").unwrap();
        assert_eq!(c.args, ["start", "--cwd", "/home/u/My Docs"]);
    }
}
