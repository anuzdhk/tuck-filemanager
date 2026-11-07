// SPDX-License-Identifier: GPL-3.0-or-later
//! Quick Look support: decide how to preview a file, read text safely, and
//! turn Markdown into simple blocks the UI renders natively.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::archive;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Image,
    /// GIF (possibly animated).
    Gif,
    Svg,
    Video,
    Audio,
    Pdf,
    Markdown,
    Text,
    Archive,
    /// Installers and app bundles (.deb, .rpm, .AppImage, .flatpakref, .exe…).
    Package,
    /// 3D models (glTF, OBJ, STL, FBX).
    Model,
    Unsupported,
}

const TEXT_MIMES: &[&str] = &[
    "application/json", "application/xml", "application/javascript", "application/x-shellscript",
    "application/toml", "application/x-yaml", "application/yaml", "application/sql", "application/x-desktop",
    "application/x-perl", "application/x-ruby", "application/x-php", "application/x-python", "application/ecmascript",
    "application/x-subrip", "application/x-trash", "application/x-zerosize", "application/x-wine-extension-ini",
    "application/x-sh", "application/xhtml+xml", "application/rss+xml", "application/atom+xml", "application/x-csh",
    "application/typescript", "application/x-awk", "application/x-m4", "application/x-gettext-translation",
];

const PACKAGE_EXTS: &[&str] = &["deb", "rpm", "appimage", "flatpakref", "flatpak", "snap", "exe", "msi", "dmg", "pkg", "apk"];

/// How to preview `path`. `mime` is the name-based guess; contents are
/// checked where the name can lie (archives, text).
pub fn classify(path: &Path, mime: &str, is_dir: bool) -> Kind {
    if is_dir {
        return Kind::Folder;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ext = crate::entry::extension_of(&name);
    if PACKAGE_EXTS.contains(&ext.as_str())
        || mime.contains("debian-package")
        || mime == "application/x-rpm"
        || mime.contains("appimage")
        || mime.contains("flatpak")
        || mime == "application/x-msdownload"
        || mime == "application/x-ms-dos-executable"
    {
        return Kind::Package;
    }
    if crate::model3d::is_model(path, mime) {
        return Kind::Model;
    }
    if mime == "image/gif" {
        return Kind::Gif;
    }
    if mime == "image/svg+xml" || mime == "image/svg+xml-compressed" {
        return Kind::Svg;
    }
    if mime.starts_with("image/") {
        return Kind::Image;
    }
    if mime.starts_with("video/") {
        return Kind::Video;
    }
    if mime.starts_with("audio/") {
        return Kind::Audio;
    }
    if mime == "application/pdf" {
        return Kind::Pdf;
    }
    if mime == "text/markdown" || matches!(ext.as_str(), "md" | "markdown" | "mdown" | "mkd") {
        return Kind::Markdown;
    }
    let fmt = archive::detect(path).ok().flatten();
    if archive::is_extractable(path, fmt) {
        return Kind::Archive;
    }
    if mime.starts_with("text/") || TEXT_MIMES.contains(&mime) || looks_like_text(path) {
        return Kind::Text;
    }
    Kind::Unsupported
}

/// UTF-8 (or ASCII) without NUL bytes in the first 8 KiB.
pub fn looks_like_text(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut buf = [0u8; 8192];
    let Ok(n) = f.read(&mut buf) else { return false };
    if n == 0 {
        return true;
    }
    let b = &buf[..n];
    if b.contains(&0) {
        return false;
    }
    match std::str::from_utf8(b) {
        Ok(_) => true,
        // A multi-byte char cut at the buffer end is fine.
        Err(e) => e.error_len().is_none() && e.valid_up_to() + 4 >= n,
    }
}

/// Read up to `max` bytes as text (lossy). Returns (text, truncated).
pub fn read_text(path: &Path, max: usize) -> std::io::Result<(String, bool)> {
    let f = File::open(path)?;
    let len = f.metadata()?.len() as usize;
    let mut buf = Vec::with_capacity(len.min(max));
    f.take(max as u64).read_to_end(&mut buf)?;
    // Don't end mid-character.
    let text = match std::str::from_utf8(&buf) {
        Ok(s) => s.to_string(),
        Err(e) if e.error_len().is_none() => String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(),
        Err(_) => String::from_utf8_lossy(&buf).into_owned(),
    };
    Ok((text, len > max))
}

// ---------------------------------------------------------------- markdown

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    /// (depth, bullet text such as "•" or "3.", content)
    ListItem(usize, String, Vec<Span>),
    Quote(Vec<Span>),
    Code(Option<String>, String),
    Rule,
    /// Rows of cells; the first row is the header.
    Table(Vec<Vec<Vec<Span>>>),
}

/// Parse CommonMark (with tables, strikethrough, task lists) into blocks.
/// Raw HTML is shown as text, never interpreted.
pub fn markdown(src: &str) -> Vec<Block> {
    use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut out = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let (mut bold, mut italic, mut strike) = (0, 0, 0);
    let mut link: Option<String> = None;
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut item_bullet: Option<String> = None;
    let mut heading: Option<u8> = None;
    let mut quote = 0;
    let mut code: Option<(Option<String>, String)> = None;
    let mut table: Option<Vec<Vec<Vec<Span>>>> = None;
    let mut row: Vec<Vec<Span>> = Vec::new();

    let push_text = |spans: &mut Vec<Span>, t: &str, bold: i32, italic: i32, strike: i32, link: &Option<String>, code: bool| {
        spans.push(Span { text: t.to_string(), bold: bold > 0, italic: italic > 0, strike: strike > 0, code, link: link.clone() });
    };
    let flush = |spans: &mut Vec<Span>, out: &mut Vec<Block>, heading: Option<u8>, quote: i32, bullet: &mut Option<String>, depth: usize| {
        if spans.is_empty() {
            return;
        }
        let s = std::mem::take(spans);
        if let Some(b) = bullet.take() {
            out.push(Block::ListItem(depth, b, s));
        } else if let Some(h) = heading {
            out.push(Block::Heading(h, s));
        } else if quote > 0 {
            out.push(Block::Quote(s));
        } else {
            out.push(Block::Paragraph(s));
        }
    };

    for ev in Parser::new_ext(src, opts) {
        if let Some((_, buf)) = code.as_mut() {
            match ev {
                Event::Text(t) => {
                    buf.push_str(&t);
                    continue;
                }
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, text) = code.take().expect("in code");
                    out.push(Block::Code(lang, text.trim_end_matches('\n').to_string()));
                    continue;
                }
                _ => continue,
            }
        }
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                })
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut spans, &mut out, heading, quote, &mut item_bullet, lists.len());
                heading = None;
            }
            Event::End(TagEnd::Paragraph) => {
                if table.is_none() {
                    flush(&mut spans, &mut out, heading, quote, &mut item_bullet, lists.len().saturating_sub(1));
                }
            }
            Event::Start(Tag::BlockQuote(_)) => quote += 1,
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut spans, &mut out, heading, quote, &mut item_bullet, 0);
                quote -= 1;
            }
            Event::Start(Tag::List(start)) => {
                flush(&mut spans, &mut out, heading, quote, &mut item_bullet, lists.len().saturating_sub(1));
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                flush(&mut spans, &mut out, heading, quote, &mut item_bullet, lists.len().saturating_sub(1));
                let bullet = match lists.last_mut() {
                    Some(Some(n)) => {
                        let b = format!("{n}.");
                        *n += 1;
                        b
                    }
                    _ => "•".to_string(),
                };
                item_bullet = Some(bullet);
            }
            Event::End(TagEnd::Item) => flush(&mut spans, &mut out, heading, quote, &mut item_bullet, lists.len().saturating_sub(1)),
            Event::TaskListMarker(done) => item_bullet = Some(if done { "☑".into() } else { "☐".into() }),
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut spans, &mut out, heading, quote, &mut item_bullet, 0);
                let lang = match kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => Some(l.split_whitespace().next().unwrap_or("").to_string()),
                    _ => None,
                };
                code = Some((lang, String::new()));
            }
            Event::Start(Tag::Strong) => bold += 1,
            Event::End(TagEnd::Strong) => bold -= 1,
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Emphasis) => italic -= 1,
            Event::Start(Tag::Strikethrough) => strike += 1,
            Event::End(TagEnd::Strikethrough) => strike -= 1,
            Event::Start(Tag::Link { dest_url, .. }) => link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => link = None,
            Event::Start(Tag::Image { dest_url, .. }) => {
                push_text(&mut spans, "🖼 ", bold, italic, strike, &Some(dest_url.to_string()), false);
            }
            Event::Text(t) => push_text(&mut spans, &t, bold, italic, strike, &link, false),
            Event::Code(t) => push_text(&mut spans, &t, bold, italic, strike, &link, true),
            Event::Html(t) | Event::InlineHtml(t) => push_text(&mut spans, &t, bold, italic, strike, &link, true),
            Event::SoftBreak => push_text(&mut spans, " ", bold, italic, strike, &link, false),
            Event::HardBreak => push_text(&mut spans, "\n", bold, italic, strike, &link, false),
            Event::Rule => out.push(Block::Rule),
            Event::Start(Tag::Table(_)) => {
                flush(&mut spans, &mut out, heading, quote, &mut item_bullet, 0);
                table = Some(Vec::new());
            }
            Event::End(TagEnd::TableCell) => row.push(std::mem::take(&mut spans)),
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.push(std::mem::take(&mut row));
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    out.push(Block::Table(t));
                }
            }
            _ => {}
        }
    }
    flush(&mut spans, &mut out, heading, quote, &mut item_bullet, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn classifies_by_content_where_names_lie() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        assert_eq!(classify(p, "inode/directory", true), Kind::Folder);
        fs::write(p.join("notes"), "plain words\nmore").unwrap();
        assert_eq!(classify(&p.join("notes"), "application/octet-stream", false), Kind::Text);
        fs::write(p.join("blob"), [0u8, 159, 146, 150]).unwrap();
        assert_eq!(classify(&p.join("blob"), "application/octet-stream", false), Kind::Unsupported);
        assert_eq!(classify(&p.join("README.md"), "text/markdown", false), Kind::Markdown);
        assert_eq!(classify(&p.join("a.png"), "image/png", false), Kind::Image);
        assert_eq!(classify(&p.join("a.gif"), "image/gif", false), Kind::Gif);
        assert_eq!(classify(&p.join("a.mp4"), "video/mp4", false), Kind::Video);
        assert_eq!(classify(&p.join("a.pdf"), "application/pdf", false), Kind::Pdf);
        assert_eq!(classify(&p.join("tool.AppImage"), "application/vnd.appimage", false), Kind::Package);
        assert_eq!(classify(&p.join("setup.exe"), "application/x-msdownload", false), Kind::Package);
        // A zip named .txt previews as an archive.
        let z = p.join("x.txt");
        let mut w = zip::ZipWriter::new(fs::File::create(&z).unwrap());
        w.start_file("a", zip::write::SimpleFileOptions::default()).unwrap();
        w.finish().unwrap();
        assert_eq!(classify(&z, "text/plain", false), Kind::Archive);
        assert_eq!(classify(&p.join("data.json"), "application/json", false), Kind::Text);
    }

    #[test]
    fn reads_text_without_splitting_characters() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("t.txt");
        fs::write(&f, "héllo wörld").unwrap();
        let (t, cut) = read_text(&f, 2).unwrap();
        assert_eq!((t.as_str(), cut), ("h", true));
        let (t, cut) = read_text(&f, 1000).unwrap();
        assert_eq!((t.as_str(), cut), ("héllo wörld", false));
    }

    #[test]
    fn markdown_blocks() {
        let md = "# Title\n\nSome **bold** and *it* and `code` and [link](https://x.y).\n\n- one\n- two\n  1. nested\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n---\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n<script>alert(1)</script>\n";
        let b = markdown(md);
        assert_eq!(b[0], Block::Heading(1, vec![Span { text: "Title".into(), ..Default::default() }]));
        let Block::Paragraph(p) = &b[1] else { panic!("{:?}", b[1]) };
        assert!(p.iter().any(|s| s.bold && s.text == "bold"));
        assert!(p.iter().any(|s| s.italic && s.text == "it"));
        assert!(p.iter().any(|s| s.code && s.text == "code"));
        assert!(p.iter().any(|s| s.link.as_deref() == Some("https://x.y")));
        assert_eq!(b[2], Block::ListItem(0, "•".into(), vec![Span { text: "one".into(), ..Default::default() }]));
        assert!(matches!(&b[4], Block::ListItem(1, n, s) if n == "1." && text(s) == "nested"), "{:?}", b[4]);
        assert!(matches!(&b[5], Block::Quote(s) if text(s) == "quoted"));
        assert_eq!(b[6], Block::Code(Some("rust".into()), "fn main() {}".into()));
        assert_eq!(b[7], Block::Rule);
        let Block::Table(t) = &b[8] else { panic!("{:?}", b[8]) };
        assert_eq!(t.len(), 2);
        assert_eq!(text(&t[1][1]), "2");
        assert!(matches!(&b[9], Block::ListItem(_, m, _) if m == "☑"));
        assert!(matches!(&b[10], Block::ListItem(_, m, _) if m == "☐"));
        // HTML is shown, never run.
        assert!(b.iter().any(|x| matches!(x, Block::Paragraph(s) if text(s).contains("<script>"))));
    }
}
