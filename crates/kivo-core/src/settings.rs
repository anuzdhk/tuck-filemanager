// SPDX-License-Identifier: GPL-3.0-or-later
//! Persistent settings: per-folder view state and the window session.
//! Stored as JSON under `$XDG_CONFIG_HOME/kivo/` (session in the state dir).

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
}

impl ViewMode {
    pub fn id(self) -> &'static str {
        match self {
            ViewMode::Icons => "icons",
            ViewMode::Compact => "compact",
            ViewMode::Details => "details",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        [ViewMode::Icons, ViewMode::Compact, ViewMode::Details].into_iter().find(|m| m.id() == s)
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
}

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
        }
    }
}

impl ViewSettings {
    pub fn zoom_index(&self) -> usize {
        match self.mode {
            ViewMode::Icons => self.zoom_icons,
            ViewMode::Compact => self.zoom_compact,
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
            ViewMode::Compact => &mut self.zoom_compact,
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
            ViewMode::Compact => self.zoom_compact = d.zoom_compact,
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

    pub fn set(&mut self, folder: &Path, v: ViewSettings) {
        if self.per_folder {
            if v == self.default {
                self.folders.remove(folder);
            } else {
                self.folders.insert(folder.to_path_buf(), v);
            }
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
        assert_eq!(f.get(Path::new("/b")).mode, ViewMode::Icons);
        // Setting back to default drops the override.
        f.set(a, f.default);
        assert!(f.folders.is_empty());
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
    fn missing_fields_use_defaults() {
        let v: ViewSettings = serde_json::from_str(r#"{"mode":"details"}"#).unwrap();
        assert_eq!(v.mode, ViewMode::Details);
        assert_eq!(v.zoom_icons, ViewSettings::default().zoom_icons);
    }
}
