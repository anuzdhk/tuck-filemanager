// SPDX-License-Identifier: GPL-3.0-or-later
//! Kivo's own folder and document icons, drawn here as SVG.
//!
//! Original artwork: a rounded folder body over a back panel with a soft
//! tab, tinted with the accent colour, with an embossed emblem for special
//! folders; documents are sheets with a folded corner, a type glyph and a
//! coloured badge naming the file type. Emblems and glyphs reuse Tabler
//! icon shapes (MIT).

use crate::search::Kind;
use crate::theme::Rgb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Folder {
    Plain,
    Home,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    Templates,
    Public,
}

impl Folder {
    pub fn id(self) -> &'static str {
        match self {
            Folder::Plain => "folder",
            Folder::Home => "home",
            Folder::Desktop => "desktop",
            Folder::Documents => "documents",
            Folder::Downloads => "downloads",
            Folder::Pictures => "pictures",
            Folder::Music => "music",
            Folder::Videos => "videos",
            Folder::Templates => "templates",
            Folder::Public => "public",
        }
    }

    fn glyph(self) -> Option<&'static str> {
        Some(match self {
            Folder::Plain => return None,
            Folder::Home => GLYPH_HOME,
            Folder::Desktop => GLYPH_DESKTOP,
            Folder::Documents => GLYPH_TEXT,
            Folder::Downloads => GLYPH_DOWNLOAD,
            Folder::Pictures => GLYPH_PHOTO,
            Folder::Music => GLYPH_MUSIC,
            Folder::Videos => GLYPH_MOVIE,
            Folder::Templates => GLYPH_DESCRIPTION,
            Folder::Public => GLYPH_LINK,
        })
    }
}

macro_rules! glyph {
    ($name:literal) => {
        include_str!(concat!("../../../data/icons/tabler-symbolic/", $name, ".svg"))
    };
}

const GLYPH_HOME: &str = glyph!("home");
const GLYPH_DESKTOP: &str = glyph!("device-desktop");
const GLYPH_TEXT: &str = glyph!("file-text");
const GLYPH_DOWNLOAD: &str = glyph!("download");
const GLYPH_PHOTO: &str = glyph!("photo");
const GLYPH_MUSIC: &str = glyph!("music");
const GLYPH_MOVIE: &str = glyph!("movie");
const GLYPH_DESCRIPTION: &str = glyph!("file-description");
const GLYPH_LINK: &str = glyph!("link");
const GLYPH_CODE: &str = glyph!("code");
const GLYPH_ZIP: &str = glyph!("file-zip");
const GLYPH_TABLE: &str = glyph!("table");
const GLYPH_PRESENTATION: &str = glyph!("presentation");
const GLYPH_PACKAGE: &str = glyph!("package");
const GLYPH_FILE: &str = glyph!("file");

/// The `d` attribute of a converted (single-path) Tabler icon.
fn path_data(svg: &str) -> &str {
    let start = svg.find(" d=\"").map(|i| i + 4).unwrap_or(0);
    let end = svg[start..].find('"').map(|i| start + i).unwrap_or(start);
    &svg[start..end]
}

fn mix(a: Rgb, b: Rgb, t: f64) -> String {
    a.mix(b, t).hex()
}

const WHITE: Rgb = Rgb { r: 1.0, g: 1.0, b: 1.0 };
const BLACK: Rgb = Rgb { r: 0.0, g: 0.0, b: 0.0 };

/// A folder icon tinted with `accent`.
pub fn folder_svg(kind: Folder, accent: Rgb) -> String {
    let light = mix(accent, WHITE, 0.5);
    let base = mix(accent, WHITE, 0.18);
    let back = mix(accent, BLACK, 0.12);
    let edge = mix(accent, BLACK, 0.35);
    let emblem = kind
        .glyph()
        .map(|g| {
            format!(
                "<g transform=\"translate(45 59) scale(1.5833)\"><path fill=\"{}\" fill-opacity=\"0.62\" d=\"{}\"/></g>",
                mix(accent, BLACK, 0.5),
                path_data(g)
            )
        })
        .unwrap_or_default();
    format!(
        concat!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"128\" height=\"128\" viewBox=\"0 0 128 128\">",
            "<defs><linearGradient id=\"b\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">",
            "<stop offset=\"0\" stop-color=\"{light}\"/><stop offset=\"1\" stop-color=\"{base}\"/></linearGradient></defs>",
            "<path d=\"M12 30Q12 21 21 21H48Q53 21 56 24L62 30H107Q116 30 116 39V52H12Z\" fill=\"{back}\"/>",
            "<rect x=\"8\" y=\"40\" width=\"112\" height=\"74\" rx=\"11\" fill=\"url(#b)\"/>",
            "<rect x=\"8.75\" y=\"40.75\" width=\"110.5\" height=\"72.5\" rx=\"10.25\" fill=\"none\" stroke=\"{edge}\" stroke-opacity=\"0.28\" stroke-width=\"1.5\"/>",
            "<path d=\"M19 45.5H109\" stroke=\"#ffffff\" stroke-opacity=\"0.45\" stroke-width=\"2\" stroke-linecap=\"round\"/>",
            "{emblem}</svg>"
        ),
        light = light,
        base = base,
        back = back,
        edge = edge,
        emblem = emblem
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Doc {
    Generic,
    Pdf,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Text,
    Document,
    Spreadsheet,
    Presentation,
    App,
}

impl Doc {
    pub fn id(self) -> &'static str {
        match self {
            Doc::Generic => "generic",
            Doc::Pdf => "pdf",
            Doc::Image => "image",
            Doc::Video => "video",
            Doc::Audio => "audio",
            Doc::Archive => "archive",
            Doc::Code => "code",
            Doc::Text => "text",
            Doc::Document => "document",
            Doc::Spreadsheet => "spreadsheet",
            Doc::Presentation => "presentation",
            Doc::App => "app",
        }
    }

    /// Badge colour; white badge text on it meets AA.
    pub fn color(self) -> Rgb {
        let hex = match self {
            Doc::Generic => "#5c6370",
            Doc::Pdf => "#c62828",
            Doc::Image => "#1e7a4c",
            Doc::Video => "#6a3fc0",
            Doc::Audio => "#b8326f",
            Doc::Archive => "#8a5a00",
            Doc::Code => "#0f6f78",
            Doc::Text => "#4a5a72",
            Doc::Document => "#2856b8",
            Doc::Spreadsheet => "#18753c",
            Doc::Presentation => "#b84b12",
            Doc::App => "#2f5fc4",
        };
        Rgb::parse(hex).expect("valid colour")
    }

    fn glyph(self) -> &'static str {
        match self {
            Doc::Generic => GLYPH_FILE,
            Doc::Pdf | Doc::Document => GLYPH_DESCRIPTION,
            Doc::Image => GLYPH_PHOTO,
            Doc::Video => GLYPH_MOVIE,
            Doc::Audio => GLYPH_MUSIC,
            Doc::Archive => GLYPH_ZIP,
            Doc::Code => GLYPH_CODE,
            Doc::Text => GLYPH_TEXT,
            Doc::Spreadsheet => GLYPH_TABLE,
            Doc::Presentation => GLYPH_PRESENTATION,
            Doc::App => GLYPH_PACKAGE,
        }
    }

    pub fn for_kind(kind: Kind) -> Doc {
        match kind {
            Kind::Image => Doc::Image,
            Kind::Video => Doc::Video,
            Kind::Audio => Doc::Audio,
            Kind::Pdf => Doc::Pdf,
            Kind::Document => Doc::Document,
            Kind::Spreadsheet => Doc::Spreadsheet,
            Kind::Presentation => Doc::Presentation,
            Kind::Archive => Doc::Archive,
            Kind::Code => Doc::Code,
            Kind::Text => Doc::Text,
            Kind::App => Doc::App,
            Kind::Folder | Kind::Other => Doc::Generic,
        }
    }
}

/// Badge text for a file name: its extension, upper-case, at most 4 letters.
pub fn badge(name: &str) -> String {
    let ext = crate::entry::extension_of(name);
    if ext.is_empty() || ext.len() > 4 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return String::new();
    }
    ext.to_ascii_uppercase()
}

/// A document icon with a type glyph and an optional badge (e.g. "PDF").
pub fn doc_svg(kind: Doc, badge: &str) -> String {
    let color = kind.color();
    let badge_svg = if badge.is_empty() {
        String::new()
    } else {
        let w = 18 + 9 * badge.chars().count() as i32;
        format!(
            "<rect x=\"{x}\" y=\"88\" width=\"{w}\" height=\"21\" rx=\"6\" fill=\"{c}\"/><text x=\"64\" y=\"103\" text-anchor=\"middle\" font-family=\"sans-serif\" font-weight=\"700\" font-size=\"12.5\" letter-spacing=\"0.5\" fill=\"#ffffff\">{t}</text>",
            x = 64 - w / 2,
            w = w,
            c = color.hex(),
            t = badge
        )
    };
    let gy = if badge.is_empty() { 50 } else { 38 };
    format!(
        concat!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"128\" height=\"128\" viewBox=\"0 0 128 128\">",
            "<path d=\"M31 7H80L107 34V112Q107 121 98 121H31Q22 121 22 112V16Q22 7 31 7Z\" fill=\"#fbfbfd\" stroke=\"#c5c9d3\" stroke-width=\"2\"/>",
            "<path d=\"M80 7V26Q80 34 88 34H107\" fill=\"#e7e9ef\" stroke=\"#c5c9d3\" stroke-width=\"2\" stroke-linejoin=\"round\"/>",
            "<g transform=\"translate(46 {gy}) scale(1.5)\"><path fill=\"{c}\" d=\"{d}\"/></g>",
            "{badge}</svg>"
        ),
        gy = gy,
        c = color.hex(),
        d = path_data(kind.glyph()),
        badge = badge_svg
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::contrast;

    const ALL_DOCS: [Doc; 12] = [
        Doc::Generic, Doc::Pdf, Doc::Image, Doc::Video, Doc::Audio, Doc::Archive, Doc::Code, Doc::Text,
        Doc::Document, Doc::Spreadsheet, Doc::Presentation, Doc::App,
    ];
    const ALL_FOLDERS: [Folder; 10] = [
        Folder::Plain, Folder::Home, Folder::Desktop, Folder::Documents, Folder::Downloads, Folder::Pictures,
        Folder::Music, Folder::Videos, Folder::Templates, Folder::Public,
    ];

    #[test]
    fn every_icon_is_valid_svg() {
        let opt = usvg::Options::default();
        for f in ALL_FOLDERS {
            for accent in ["#3584e4", "#c88800", "#6f8396", "#ffffff", "#000000"] {
                let svg = folder_svg(f, Rgb::parse(accent).unwrap());
                usvg::Tree::from_str(&svg, &opt).unwrap_or_else(|e| panic!("{f:?}: {e}"));
            }
        }
        for d in ALL_DOCS {
            for b in ["", "PDF", "JPEG"] {
                usvg::Tree::from_str(&doc_svg(d, b), &opt).unwrap_or_else(|e| panic!("{d:?}: {e}"));
            }
        }
    }

    #[test]
    fn badges_are_readable() {
        for d in ALL_DOCS {
            let c = contrast(Rgb::parse("#ffffff").unwrap(), d.color());
            assert!(c >= 4.5, "{d:?} badge text contrast {c:.2}");
        }
        assert_eq!(badge("Report.PDF"), "PDF");
        assert_eq!(badge("archive.tar.gz"), "GZ");
        assert_eq!(badge("Makefile"), "");
        assert_eq!(badge("x.verylongext"), "");
    }

    #[test]
    fn glyphs_are_present() {
        for d in ALL_DOCS {
            assert!(path_data(d.glyph()).starts_with('M'), "{d:?}");
        }
        for f in ALL_FOLDERS {
            if let Some(g) = f.glyph() {
                assert!(path_data(g).starts_with('M'), "{f:?}");
            }
        }
        assert_eq!(Doc::for_kind(Kind::Pdf), Doc::Pdf);
    }
}
