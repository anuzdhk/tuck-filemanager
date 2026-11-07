// SPDX-License-Identifier: GPL-3.0-or-later
//! Addons: optional features that can be turned off in Preferences. Each is
//! listed once in [`ALL`]; its switch lives in `AppSettings::addons` (absent
//! means the addon's default). To add one: a new entry here, then check
//! [`enabled`] wherever the feature shows up in the UI.

use crate::settings::AppSettings;

pub struct Addon {
    /// Stable key in settings.json.
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    /// Programs it runs; any one of each group is enough (e.g. `magick` or
    /// ImageMagick 6's `convert`).
    pub needs: &'static [&'static [&'static str]],
    /// What to install when a program is missing.
    pub install_hint: &'static str,
    pub default_on: bool,
}

pub const IMAGE_CONVERTER: &str = "image-converter";
pub const VIDEO_COMPRESSOR: &str = "video-compressor";
pub const MODEL_VIEWER: &str = "model-viewer";

pub const ALL: &[Addon] = &[
    Addon {
        id: IMAGE_CONVERTER,
        name: "Image Converter",
        summary: "Convert and compress images, or every image in a folder, to WebP, AVIF, JPEG or PNG. Right-click → Convert Images…",
        needs: &[&["magick", "convert"]],
        install_hint: "Install ImageMagick (the “imagemagick” package).",
        default_on: true,
    },
    Addon {
        id: VIDEO_COMPRESSOR,
        name: "Video Compressor",
        summary: "Shrink videos with simple presets, no extra app needed. Right-click → Compress Video…",
        needs: &[&["ffmpeg"]],
        install_hint: "Install FFmpeg (the “ffmpeg” package).",
        default_on: true,
    },
    Addon {
        id: MODEL_VIEWER,
        name: "3D Model Viewer",
        summary: "Thumbnails and Quick Look for GLB, glTF, OBJ, STL and FBX models. Drag to turn the model.",
        needs: &[],
        install_hint: "",
        default_on: true,
    },
];

pub fn find(id: &str) -> Option<&'static Addon> {
    ALL.iter().find(|a| a.id == id)
}

/// Is addon `id` switched on?
pub fn enabled(settings: &AppSettings, id: &str) -> bool {
    settings.addons.get(id).copied().unwrap_or_else(|| find(id).is_some_and(|a| a.default_on))
}

/// The first program of each group that's installed; None if one group has
/// none (the addon can't run).
pub fn programs(addon: &Addon) -> Option<Vec<&'static str>> {
    addon.needs.iter().map(|group| group.iter().copied().find(|p| crate::terminal::on_path(p))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let mut s = AppSettings::default();
        assert!(enabled(&s, MODEL_VIEWER));
        s.addons.insert(MODEL_VIEWER.into(), false);
        assert!(!enabled(&s, MODEL_VIEWER));
        assert!(!enabled(&s, "no-such-addon"));
        // Ids are unique (they're settings keys).
        for (i, a) in ALL.iter().enumerate() {
            assert!(ALL[i + 1..].iter().all(|b| b.id != a.id));
        }
        assert_eq!(programs(find(MODEL_VIEWER).unwrap()), Some(vec![]));
    }
}
