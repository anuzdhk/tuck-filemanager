// SPDX-License-Identifier: GPL-3.0-or-later
//! Persistent settings: per-folder view state and the window session.
//! Stored as JSON under `$XDG_CONFIG_HOME/tuck/` (session in the state dir).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sort::SortOptions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ViewMode {
    #[default]
    Icons,
    Compact,
    Details,
    /// macOS-style columns: each selected folder opens in the next column.
    Columns,
}

impl ViewMode {
    pub fn id(self) -> &'static str {
        match self {
            ViewMode::Icons => "icons",
            ViewMode::Compact => "compact",
            ViewMode::Details => "details",
            ViewMode::Columns => "columns",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        [ViewMode::Icons, ViewMode::Compact, ViewMode::Details, ViewMode::Columns].into_iter().find(|m| m.id() == s)
    }
}

/// Zoom steps for each view, in icon pixels.
pub const ZOOM_LEVELS: [u32; 7] = [16, 24, 32, 48, 64, 96, 128];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewSettings {
    pub mode: ViewMode,
    pub sort: SortOptions,
    pub show_hidden: bool,
    pub grouped: bool,
    /// Index into [`ZOOM_LEVELS`], separately per mode.
    pub zoom_icons: usize,
    pub zoom_compact: usize,
    pub zoom_details: usize,
    /// Show thumbnails instead of icons where possible.
    pub previews: bool,
    /// Details columns shown, as bits of [`COLUMNS`].
    pub columns: u8,
}

/// Optional Details columns: (id, title, bit). Name is always shown.
pub const COLUMNS: [(&str, &str, u8); 6] = [
    ("size", "Size", 1),
    ("modified", "Date Modified", 2),
    ("type", "Kind", 4),
    ("created", "Date Created", 8),
    ("permissions", "Permissions", 16),
    ("owner", "Owner", 32),
];

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            mode: ViewMode::Icons,
            sort: SortOptions::default(),
            show_hidden: false,
            grouped: false,
            zoom_icons: 4,
            zoom_compact: 1,
            zoom_details: 1,
            previews: true,
            columns: 1 | 2 | 4,
        }
    }
}

impl ViewSettings {
    pub fn has_column(&self, id: &str) -> bool {
        COLUMNS.iter().find(|(c, ..)| *c == id).is_some_and(|(_, _, bit)| self.columns & bit != 0)
    }

    pub fn set_column(&mut self, id: &str, on: bool) {
        if let Some((_, _, bit)) = COLUMNS.iter().find(|(c, ..)| *c == id) {
            if on { self.columns |= bit } else { self.columns &= !bit }
        }
    }

    pub fn zoom_index(&self) -> usize {
        match self.mode {
            ViewMode::Icons => self.zoom_icons,
            // Columns share Compact's zoom: both are small-icon lists.
            ViewMode::Compact | ViewMode::Columns => self.zoom_compact,
            ViewMode::Details => self.zoom_details,
        }
    }

    pub fn icon_size(&self) -> u32 {
        ZOOM_LEVELS[self.zoom_index().min(ZOOM_LEVELS.len() - 1)]
    }

    /// Change zoom by `delta` steps; returns whether it changed.
    pub fn zoom_by(&mut self, delta: i32) -> bool {
        let slot = match self.mode {
            ViewMode::Icons => &mut self.zoom_icons,
            ViewMode::Compact | ViewMode::Columns => &mut self.zoom_compact,
            ViewMode::Details => &mut self.zoom_details,
        };
        let new = (*slot as i32 + delta).clamp(0, ZOOM_LEVELS.len() as i32 - 1) as usize;
        let changed = new != *slot;
        *slot = new;
        changed
    }

    pub fn zoom_reset(&mut self) {
        let d = Self::default();
        match self.mode {
            ViewMode::Icons => self.zoom_icons = d.zoom_icons,
            ViewMode::Compact | ViewMode::Columns => self.zoom_compact = d.zoom_compact,
            ViewMode::Details => self.zoom_details = d.zoom_details,
        }
    }
}

/// View settings remembered per folder, with a global default. Matches
/// Dolphin's "Remember display style for each folder" option.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderViews {
    pub per_folder: bool,
    pub default: ViewSettings,
    pub folders: HashMap<PathBuf, ViewSettings>,
}

impl FolderViews {
    pub fn new() -> Self {
        Self { per_folder: true, ..Default::default() }
    }

    pub fn get(&self, folder: &Path) -> ViewSettings {
        if self.per_folder
            && let Some(v) = self.folders.get(folder) {
                return *v;
            }
        self.default
    }

    /// Remember `v` for this folder. Its view mode and sort order also become
    /// the default for folders that have no settings of their own, so the
    /// next new folder opens the way you last chose (e.g. Columns).
    pub fn set(&mut self, folder: &Path, v: ViewSettings) {
        if self.per_folder {
            self.folders.insert(folder.to_path_buf(), v);
            self.default.mode = v.mode;
            self.default.sort = v.sort;
        } else {
            self.default = v;
        }
    }

    /// "Apply to all folders": make `v` the default and forget overrides.
    pub fn apply_to_all(&mut self, v: ViewSettings) {
        self.default = v;
        self.folders.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum IconStyle {
    /// Tabler UI icons with Tuck's own folder and document icons.
    #[default]
    Normal,
    /// Material Symbols (rounded), coloured with the accent.
    Material,
    /// The desktop's own icon theme (Adwaita, Yaru, Papirus, Breeze…).
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Density {
    Compact,
    #[default]
    Comfortable,
    Spacious,
}

/// Appearance and accessibility settings (`~/.config/tuck/settings.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// "system", a built-in theme id, or "user:<file name>".
    pub theme: String,
    /// Accent preset id, "theme", or a custom "#rrggbb".
    pub accent: String,
    pub icon_style: IconStyle,
    /// Text size multiplier (0.8–1.6).
    pub text_scale: f64,
    pub density: Density,
    pub reduce_motion: bool,
    /// Custom keyboard shortcuts: action → accelerators (empty = none).
    /// Actions not listed keep their defaults.
    pub shortcuts: std::collections::BTreeMap<String, Vec<String>>,
    /// Look for a new version once a day, at startup.
    pub check_updates: bool,
    /// Last automatic check (Unix seconds).
    pub last_update_check: i64,
    /// Sidebar width in pixels, set by dragging its edge.
    pub sidebar_width: f64,
    /// Sidebar sections folded away (by id: "favourites", "drives", …).
    pub sidebar_collapsed: Vec<String>,
    /// Addon switches by id (see `addons`); missing = the addon's default.
    pub addons: std::collections::BTreeMap<String, bool>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            accent: "theme".into(),
            icon_style: IconStyle::Normal,
            text_scale: 1.0,
            density: Density::Comfortable,
            reduce_motion: false,
            shortcuts: Default::default(),
            check_updates: true,
            last_update_check: 0,
            sidebar_width: 240.0,
            sidebar_collapsed: Vec::new(),
            addons: Default::default(),
        }
    }
}

impl AppSettings {
    /// Clamp values that may have been edited by hand.
    pub fn sanitized(mut self) -> Self {
        if !self.text_scale.is_finite() {
            self.text_scale = 1.0;
        }
        self.text_scale = self.text_scale.clamp(0.8, 1.6);
        if self.accent.starts_with('#') && crate::theme::Rgb::parse(&self.accent).is_none() {
            self.accent = "theme".into();
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabState {
    pub left: PathBuf,
    pub right: Option<PathBuf>,
    pub right_active: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<TabState>,
    pub active: usize,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    pub sidebar_visible: Option<bool>,
    pub info_visible: Option<bool>,
}

/// Read JSON from `path`. A missing file gives the default; a corrupt one is
/// moved aside (never silently overwritten) and gives the default.
pub fn load_json<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> T {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => {
                let _ = fs::rename(path, path.with_extension("json.corrupt"));
                T::default()
            }
        },
        Err(_) => T::default(),
    }
}

/// Write JSON atomically: temp file in the same directory, then rename.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_clamps() {
        let mut v = ViewSettings::default();
        assert!(!{
            let mut c = v;
            c.zoom_by(100);
            c.zoom_by(1)
        });
        v.zoom_by(-100);
        assert_eq!(v.icon_size(), ZOOM_LEVELS[0]);
        v.zoom_reset();
        assert_eq!(v, ViewSettings::default());
    }

    #[test]
    fn zoom_is_per_mode() {
        let mut v = ViewSettings::default();
        v.zoom_by(1);
        v.mode = ViewMode::Details;
        assert_eq!(v.zoom_index(), ViewSettings::default().zoom_details);
    }

    #[test]
    fn per_folder_settings() {
        let mut f = FolderViews::new();
        let a = Path::new("/a");
        let mut v = f.get(a);
        v.mode = ViewMode::Details;
        f.set(a, v);
        assert_eq!(f.get(a).mode, ViewMode::Details);
        // New folders follow the last choice; /a keeps its own.
        assert_eq!(f.get(Path::new("/b")).mode, ViewMode::Details);
        let mut c = f.get(Path::new("/c"));
        c.mode = ViewMode::Columns;
        c.sort.descending = true;
        f.set(Path::new("/c"), c);
        assert_eq!(f.get(a).mode, ViewMode::Details);
        assert_eq!(f.get(Path::new("/b")).mode, ViewMode::Columns);
        assert!(f.get(Path::new("/b")).sort.descending);
        f.set(a, v);
        f.apply_to_all(v);
        assert!(f.folders.is_empty());
        assert_eq!(f.get(Path::new("/b")).mode, ViewMode::Details);
    }

    #[test]
    fn json_roundtrip_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/views.json");
        let mut f = FolderViews::new();
        f.set(Path::new("/x"), ViewSettings { show_hidden: true, ..Default::default() });
        save_json(&p, &f).unwrap();
        let back: FolderViews = load_json(&p);
        assert!(back.get(Path::new("/x")).show_hidden);

        fs::write(&p, b"{not json").unwrap();
        let fresh: FolderViews = load_json(&p);
        assert!(fresh.folders.is_empty());
        assert!(p.with_extension("json.corrupt").exists(), "corrupt file kept aside");
    }

    #[test]
    fn app_settings_defaults_and_sanitizing() {
        let s: AppSettings = serde_json::from_str(r##"{"theme":"nord","text_scale":9.0,"accent":"#zzz"}"##).unwrap();
        let s = s.sanitized();
        assert_eq!(s.theme, "nord");
        assert_eq!(s.text_scale, 1.6);
        assert_eq!(s.accent, "theme");
        assert_eq!(s.icon_style, IconStyle::Normal);
        let json = serde_json::to_string(&AppSettings::default()).unwrap();
        assert!(json.contains("\"icon_style\":\"normal\"") && json.contains("comfortable"));
    }

    #[test]
    fn missing_fields_use_defaults() {
        let v: ViewSettings = serde_json::from_str(r#"{"mode":"details"}"#).unwrap();
        assert_eq!(v.mode, ViewMode::Details);
        assert_eq!(v.zoom_icons, ViewSettings::default().zoom_icons);
    }
}
