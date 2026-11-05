// SPDX-License-Identifier: GPL-3.0-or-later
//! Search: a persistent name index for instant results, a streaming live
//! search for places the index doesn't cover, and full-text content search.
//!
//! Queries are plain words plus optional filters, typed or set from the UI:
//! `report kind:pdf modified:month size:>2mb tag:work in:~/Documents`

pub mod content;
pub mod index;
pub mod live;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Coarse file kinds, decided by extension so the index never opens files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Kind {
    Other = 0,
    Folder = 1,
    Image = 2,
    Video = 3,
    Audio = 4,
    Document = 5,
    Pdf = 6,
    Spreadsheet = 7,
    Presentation = 8,
    Archive = 9,
    Code = 10,
    Text = 11,
    App = 12,
}

impl Kind {
    pub const FILTERS: [Kind; 12] = [
        Kind::Folder, Kind::Image, Kind::Video, Kind::Audio, Kind::Document, Kind::Pdf, Kind::Spreadsheet,
        Kind::Presentation, Kind::Archive, Kind::Code, Kind::Text, Kind::App,
    ];

    pub fn from_u8(v: u8) -> Kind {
        Kind::FILTERS.into_iter().find(|k| *k as u8 == v).unwrap_or(Kind::Other)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Other => "Other",
            Kind::Folder => "Folders",
            Kind::Image => "Images",
            Kind::Video => "Videos",
            Kind::Audio => "Music & Audio",
            Kind::Document => "Documents",
            Kind::Pdf => "PDFs",
            Kind::Spreadsheet => "Spreadsheets",
            Kind::Presentation => "Presentations",
            Kind::Archive => "Archives",
            Kind::Code => "Code",
            Kind::Text => "Text",
            Kind::App => "Apps & Installers",
        }
    }

    pub fn keyword(self) -> &'static str {
        match self {
            Kind::Other => "other",
            Kind::Folder => "folder",
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Document => "document",
            Kind::Pdf => "pdf",
            Kind::Spreadsheet => "spreadsheet",
            Kind::Presentation => "presentation",
            Kind::Archive => "archive",
            Kind::Code => "code",
            Kind::Text => "text",
            Kind::App => "app",
        }
    }

    fn from_keyword(s: &str) -> Option<Kind> {
        let s = s.trim_end_matches('s');
        Kind::FILTERS.into_iter().find(|k| k.keyword() == s).or(match s {
            "dir" | "directorie" => Some(Kind::Folder),
            "photo" | "picture" | "img" => Some(Kind::Image),
            "movie" => Some(Kind::Video),
            "music" | "song" => Some(Kind::Audio),
            "doc" => Some(Kind::Document),
            "sheet" => Some(Kind::Spreadsheet),
            "slide" => Some(Kind::Presentation),
            "zip" => Some(Kind::Archive),
            "source" => Some(Kind::Code),
            _ => None,
        })
    }

    /// Kind from a file name (and whether it's a folder).
    pub fn of(name: &str, is_dir: bool) -> Kind {
        if is_dir {
            return Kind::Folder;
        }
        let ext = crate::entry::extension_of(name);
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".tar.gz") || lower.ends_with(".tar.xz") || lower.ends_with(".tar.zst") || lower.ends_with(".tar.bz2") {
            return Kind::Archive;
        }
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "svg" | "bmp" | "tif" | "tiff" | "ico"
            | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "rw2" | "jxl" | "xcf" | "psd" | "kra" => Kind::Image,
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "3gp" | "ogv" | "m2ts" => Kind::Video,
            "mp3" | "flac" | "ogg" | "oga" | "opus" | "wav" | "m4a" | "aac" | "wma" | "aiff" | "aif" | "mid" | "midi" => Kind::Audio,
            "pdf" => Kind::Pdf,
            "doc" | "docx" | "odt" | "rtf" | "pages" | "epub" | "mobi" | "tex" | "wpd" => Kind::Document,
            "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => Kind::Spreadsheet,
            "ppt" | "pptx" | "odp" | "key" => Kind::Presentation,
            "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz" | "lzma" | "cab" | "iso" => Kind::Archive,
            "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "java" | "kt" | "go"
            | "rb" | "php" | "swift" | "cs" | "sh" | "bash" | "zsh" | "fish" | "lua" | "pl" | "r" | "scala" | "dart"
            | "zig" | "nim" | "hs" | "ml" | "ex" | "exs" | "erl" | "clj" | "vue" | "svelte" | "html" | "css" | "scss"
            | "sql" | "json" | "yaml" | "yml" | "toml" | "xml" | "ini" | "cmake" | "mk" | "gradle" | "ipynb" => Kind::Code,
            "txt" | "md" | "markdown" | "log" | "rst" | "org" | "nfo" | "srt" | "vtt" => Kind::Text,
            "appimage" | "deb" | "rpm" | "flatpakref" | "flatpak" | "snap" | "exe" | "msi" | "dmg" | "pkg" | "apk" | "desktop" => Kind::App,
            _ => match lower.as_str() {
                "makefile" | "dockerfile" | "cargo.lock" | "license" | "readme" => Kind::Code,
                _ => Kind::Other,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateFilter {
    Today,
    Week,
    Month,
    Year,
}

impl DateFilter {
    pub const ALL: [DateFilter; 4] = [DateFilter::Today, DateFilter::Week, DateFilter::Month, DateFilter::Year];

    pub fn label(self) -> &'static str {
        match self {
            DateFilter::Today => "Today",
            DateFilter::Week => "Past Week",
            DateFilter::Month => "Past Month",
            DateFilter::Year => "Past Year",
        }
    }

    /// Earliest modification time (unix seconds) that passes, given `now`.
    pub fn since(self, now: i64) -> i64 {
        match self {
            DateFilter::Today => {
                let local = chrono::Local::now().date_naive().and_hms_opt(0, 0, 0).expect("midnight");
                local.and_local_timezone(chrono::Local).earliest().map(|t| t.timestamp()).unwrap_or(now - 86_400)
            }
            DateFilter::Week => now - 7 * 86_400,
            DateFilter::Month => now - 30 * 86_400,
            DateFilter::Year => now - 365 * 86_400,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Lowercased words; all must appear in the name (or the path, for
    /// words containing '/').
    pub terms: Vec<String>,
    pub kinds: Vec<Kind>,
    pub modified: Option<DateFilter>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub tags: Vec<String>,
    /// Only items inside this folder.
    pub location: Option<PathBuf>,
    /// Include hidden files and folders (and items inside them).
    pub hidden: bool,
}

impl Query {
    /// Parse typed text. Unknown `key:value` words are treated as plain words.
    pub fn parse(text: &str, home: &Path) -> Query {
        let mut q = Query::default();
        for word in text.split_whitespace() {
            if let Some((k, v)) = word.split_once(':')
                && !v.is_empty()
            {
                {
                    match k.to_ascii_lowercase().as_str() {
                        "kind" | "type" => {
                            if let Some(kind) = Kind::from_keyword(&v.to_ascii_lowercase()) {
                                q.kinds.push(kind);
                                continue;
                            }
                        }
                        "modified" | "date" | "changed" => {
                            let d = match v.to_ascii_lowercase().as_str() {
                                "today" => Some(DateFilter::Today),
                                "week" => Some(DateFilter::Week),
                                "month" => Some(DateFilter::Month),
                                "year" => Some(DateFilter::Year),
                                _ => None,
                            };
                            if d.is_some() {
                                q.modified = d;
                                continue;
                            }
                        }
                        "size" => {
                            if let Some(s) = v.strip_prefix('>').and_then(parse_size) {
                                q.min_size = Some(s);
                                continue;
                            }
                            if let Some(s) = v.strip_prefix('<').and_then(parse_size) {
                                q.max_size = Some(s);
                                continue;
                            }
                        }
                        "tag" => {
                            q.tags.push(v.to_lowercase());
                            continue;
                        }
                        "in" => {
                            q.location = Some(crate::paths::expand_input(v, home, home));
                            continue;
                        }
                        "hidden" => {
                            q.hidden = matches!(v, "yes" | "true" | "1");
                            continue;
                        }
                        _ => {}
                    }
                }
            }
            q.terms.push(word.to_lowercase());
        }
        q
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.kinds.is_empty() && self.modified.is_none() && self.min_size.is_none() && self.max_size.is_none() && self.tags.is_empty()
    }

    /// Do the non-text filters accept this entry?
    pub fn accepts(&self, kind: Kind, size: u64, mtime: i64, is_dir: bool, since: Option<i64>) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&kind) {
            return false;
        }
        if since.is_some_and(|s| mtime < s) {
            return false;
        }
        if !is_dir {
            if self.min_size.is_some_and(|m| size < m) || self.max_size.is_some_and(|m| size > m) {
                return false;
            }
        } else if self.min_size.is_some() || self.max_size.is_some() {
            return false;
        }
        true
    }
}

/// "10mb", "1.5G", "200k", "512" (bytes). Decimal units, like the UI shows.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let unit = s[digits.len()..].trim_end_matches('b');
    let n: f64 = digits.parse().ok()?;
    let mult = match unit {
        "" => 1.0,
        "k" => 1e3,
        "m" => 1e6,
        "g" => 1e9,
        "t" => 1e12,
        _ => return None,
    };
    Some((n * mult) as u64)
}

/// How well `name` (lowercased) matches the first term; higher is better.
/// The rest of ranking (depth, hidden, recency) is added by the caller.
pub fn name_score(name: &str, term: &str) -> i32 {
    if term.is_empty() {
        return 0;
    }
    let stem = crate::names::split_ext(name).0;
    if name == term || stem == term {
        return 1000;
    }
    if name.starts_with(term) {
        return 700;
    }
    match name.find(term) {
        Some(i) => {
            let before = name[..i].chars().next_back();
            if before.is_some_and(|c| !c.is_alphanumeric()) {
                450 // start of a word: "annual-report" for "report"
            } else {
                200
            }
        }
        None => 0,
    }
}

/// Final rank for a match. `depth` counts path components below the root.
pub fn rank(base: i32, name_len: usize, depth: usize, hidden: bool, is_dir: bool, mtime: i64, now: i64) -> i32 {
    let mut s = base;
    s -= (depth as i32) * 6;
    s -= (name_len as i32) / 3;
    if hidden {
        s -= 400;
    }
    if is_dir {
        s += 25;
    }
    let age = now - mtime;
    if age < 7 * 86_400 {
        s += 90;
    } else if age < 30 * 86_400 {
        s += 45;
    } else if age < 365 * 86_400 {
        s += 10;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_queries() {
        let home = Path::new("/home/u");
        let q = Query::parse("Annual Report kind:pdf modified:month size:>2mb tag:Work in:~/Documents", home);
        assert_eq!(q.terms, ["annual", "report"]);
        assert_eq!(q.kinds, [Kind::Pdf]);
        assert_eq!(q.modified, Some(DateFilter::Month));
        assert_eq!(q.min_size, Some(2_000_000));
        assert_eq!(q.tags, ["work"]);
        assert_eq!(q.location.as_deref(), Some(Path::new("/home/u/Documents")));
        let q = Query::parse("kind:photos size:<1.5k c++ note:", home);
        assert_eq!(q.kinds, [Kind::Image]);
        assert_eq!(q.max_size, Some(1500));
        assert_eq!(q.terms, ["c++", "note:"]);
        assert!(Query::parse("", home).is_empty());
        assert!(Query::parse("kind:nonsense", home).terms == ["kind:nonsense"]);
    }

    #[test]
    fn kinds_by_name() {
        assert_eq!(Kind::of("IMG_0001.JPG", false), Kind::Image);
        assert_eq!(Kind::of("backup.tar.gz", false), Kind::Archive);
        assert_eq!(Kind::of("main.rs", false), Kind::Code);
        assert_eq!(Kind::of("Makefile", false), Kind::Code);
        assert_eq!(Kind::of("Photos", true), Kind::Folder);
        assert_eq!(Kind::of("app.AppImage", false), Kind::App);
        assert_eq!(Kind::of("mystery", false), Kind::Other);
        for k in Kind::FILTERS {
            assert_eq!(Kind::from_u8(k as u8), k);
        }
    }

    #[test]
    fn filters() {
        let q = Query { kinds: vec![Kind::Image], min_size: Some(100), ..Default::default() };
        assert!(q.accepts(Kind::Image, 200, 0, false, None));
        assert!(!q.accepts(Kind::Image, 50, 0, false, None));
        assert!(!q.accepts(Kind::Video, 200, 0, false, None));
        let q = Query { min_size: Some(1), ..Default::default() };
        assert!(!q.accepts(Kind::Folder, 0, 0, true, None), "size filters exclude folders");
        let q = Query::default();
        assert!(!q.accepts(Kind::Text, 0, 10, false, Some(20)));
    }

    #[test]
    fn scoring_prefers_better_matches() {
        assert!(name_score("report.pdf", "report") > name_score("reports-2024.pdf", "report"));
        assert!(name_score("reports-2024.pdf", "report") > name_score("annual-report.pdf", "report"));
        assert!(name_score("annual-report.pdf", "report") > name_score("myreport.pdf", "report"));
        assert_eq!(name_score("other", "report"), 0);
        let now = 1_000_000_000;
        assert!(rank(700, 10, 2, false, false, now, now) > rank(700, 10, 8, false, false, now, now), "shallower first");
        assert!(rank(700, 10, 2, false, false, now, now) > rank(700, 10, 2, true, false, now, now), "hidden last");
        assert!(rank(450, 10, 2, false, false, now, now) > rank(450, 10, 2, false, false, now - 400 * 86_400, now), "recent first");
        assert_eq!(parse_size("10MB"), Some(10_000_000));
        assert_eq!(parse_size("x"), None);
    }
}
