// SPDX-License-Identifier: GPL-3.0-or-later
//! Thumbnails, following the freedesktop.org Thumbnail Managing Standard so
//! the cache is shared with Nautilus, Dolphin and every other compliant app.
//!
//! - `$XDG_CACHE_HOME/thumbnails/{normal,large,x-large,xx-large}/<md5(uri)>.png`
//! - A thumbnail is valid only if its `Thumb::URI` and `Thumb::MTime` PNG text
//!   chunks match the file.
//! - Files that can't be thumbnailed get a marker in `fail/kivo-<ver>/`, so we
//!   don't retry them on every scroll.
//! - Thumbnails are written to a temporary name and renamed into place, with
//!   0600 permissions.
//! - Raster images are scaled in-process; everything else (video, PDF, SVG,
//!   fonts…) goes through the system's `.thumbnailer` entries.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufWriter};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use md5::{Digest, Md5};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Size {
    Normal,
    Large,
    XLarge,
    XXLarge,
}

impl Size {
    pub fn pixels(self) -> u32 {
        match self {
            Size::Normal => 128,
            Size::Large => 256,
            Size::XLarge => 512,
            Size::XXLarge => 1024,
        }
    }

    pub fn dir(self) -> &'static str {
        match self {
            Size::Normal => "normal",
            Size::Large => "large",
            Size::XLarge => "x-large",
            Size::XXLarge => "xx-large",
        }
    }

    /// Smallest standard size that covers `px` on screen.
    pub fn for_pixels(px: u32) -> Size {
        match px {
            0..=128 => Size::Normal,
            129..=256 => Size::Large,
            257..=512 => Size::XLarge,
            _ => Size::XXLarge,
        }
    }
}

pub fn cache_root() -> PathBuf {
    crate::paths::cache_home().join("thumbnails")
}

/// `file:///…` URI for a local path, percent-encoded like GIO does.
pub fn uri_for(path: &Path) -> String {
    format!("file://{}", crate::trash::encode_path(path))
}

pub fn hash(uri: &str) -> String {
    let d = Md5::digest(uri.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn cache_path(root: &Path, uri: &str, size: Size) -> PathBuf {
    root.join(size.dir()).join(format!("{}.png", hash(uri)))
}

fn fail_path(root: &Path, uri: &str) -> PathBuf {
    root.join("fail").join(concat!("kivo-", env!("CARGO_PKG_VERSION"))).join(format!("{}.png", hash(uri)))
}

/// Read the tEXt chunks of a PNG (only the header area, cheaply).
pub fn png_text(path: &Path) -> Option<HashMap<String, String>> {
    let data = fs::read(path).ok()?;
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut out = HashMap::new();
    let mut i = 8;
    while i + 8 <= data.len() {
        let len = u32::from_be_bytes(data[i..i + 4].try_into().ok()?) as usize;
        let kind = &data[i + 4..i + 8];
        let body = data.get(i + 8..i + 8 + len)?;
        match kind {
            b"tEXt" => {
                if let Some(z) = body.iter().position(|&b| b == 0) {
                    let k = String::from_utf8_lossy(&body[..z]).into_owned();
                    let v = String::from_utf8_lossy(&body[z + 1..]).into_owned();
                    out.insert(k, v);
                }
            }
            b"iTXt" => {
                // keyword \0 flag method lang \0 translated \0 text
                let mut parts = body.splitn(2, |&b| b == 0);
                let k = parts.next().map(|b| String::from_utf8_lossy(b).into_owned());
                if let (Some(k), Some(rest)) = (k, parts.next())
                    && rest.len() > 2 && rest[0] == 0 {
                        let rest = &rest[2..];
                        let mut f = rest.splitn(3, |&b| b == 0);
                        let _lang = f.next();
                        let _tr = f.next();
                        if let Some(text) = f.next() {
                            out.insert(k, String::from_utf8_lossy(text).into_owned());
                        }
                    }
            }
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        i += 12 + len;
    }
    Some(out)
}

fn mtime(path: &Path) -> Option<i64> {
    fs::metadata(path).ok().map(|m| m.mtime())
}

/// Is `thumb` a valid thumbnail for `uri` with modification time `mt`?
pub fn is_valid(thumb: &Path, uri: &str, mt: i64) -> bool {
    match png_text(thumb) {
        Some(t) => t.get("Thumb::URI").map(String::as_str) == Some(uri) && t.get("Thumb::MTime").and_then(|v| v.parse::<i64>().ok()) == Some(mt),
        None => false,
    }
}

/// A `.thumbnailer` entry (from /usr/share/thumbnailers and friends).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnailer {
    pub exec: String,
    pub try_exec: Option<String>,
    pub mime_types: Vec<String>,
}

impl Thumbnailer {
    pub fn parse(text: &str) -> Option<Thumbnailer> {
        let mut in_entry = false;
        let (mut exec, mut try_exec, mut mimes) = (None, None, Vec::new());
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Thumbnailer Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some(v) = line.strip_prefix("Exec=") {
                exec = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("TryExec=") {
                try_exec = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("MimeType=") {
                mimes = v.split(';').filter(|m| !m.is_empty()).map(str::to_string).collect();
            }
        }
        Some(Thumbnailer { exec: exec?, try_exec, mime_types: mimes })
    }

    /// Expand %i %u %o %s (and %%) into an argv.
    pub fn argv(&self, input: &Path, uri: &str, output: &Path, size: u32) -> Vec<String> {
        split_exec(&self.exec)
            .into_iter()
            .map(|arg| {
                let mut out = String::new();
                let mut chars = arg.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '%' {
                        match chars.next() {
                            Some('i') => out.push_str(&input.to_string_lossy()),
                            Some('u') => out.push_str(uri),
                            Some('o') => out.push_str(&output.to_string_lossy()),
                            Some('s') => out.push_str(&size.to_string()),
                            Some('%') => out.push('%'),
                            Some(other) => {
                                out.push('%');
                                out.push(other);
                            }
                            None => out.push('%'),
                        }
                    } else {
                        out.push(c);
                    }
                }
                out
            })
            .collect()
    }

    fn available(&self) -> bool {
        let bin = self.try_exec.clone().or_else(|| split_exec(&self.exec).into_iter().next());
        match bin {
            Some(b) if b.starts_with('/') => Path::new(&b).exists(),
            Some(b) => crate::terminal::on_path(&b),
            None => false,
        }
    }
}

/// Desktop-entry style argument splitting (double quotes, backslash escapes).
pub(crate) fn split_exec(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars();
    let mut any = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// All installed thumbnailers, keyed by MIME type. User entries win.
pub fn load_thumbnailers() -> HashMap<String, Thumbnailer> {
    let mut dirs: Vec<PathBuf> = vec![crate::paths::data_home().join("thumbnailers")];
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(sys.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join("thumbnailers")));
    let mut map = HashMap::new();
    for d in dirs {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "thumbnailer")).collect();
        files.sort();
        for f in files {
            let Some(t) = fs::read_to_string(&f).ok().and_then(|s| Thumbnailer::parse(&s)) else { continue };
            if !t.available() {
                continue;
            }
            for m in &t.mime_types {
                map.entry(m.clone()).or_insert_with(|| t.clone());
            }
        }
    }
    map
}

/// MIME types decoded in-process.
pub fn decodes_natively(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/x-bmp" | "image/tiff" | "image/x-icon" | "image/vnd.microsoft.icon"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Path to a valid PNG thumbnail.
    Ready(PathBuf),
    /// Known not to be thumbnail-able (recorded in the fail cache).
    Failed,
    /// No way to thumbnail this type.
    Unsupported,
}

pub struct Thumbnails {
    root: PathBuf,
    thumbnailers: HashMap<String, Thumbnailer>,
    /// Skip files larger than this for in-process decoding.
    pub max_native_bytes: u64,
    pub timeout: Duration,
}

impl Thumbnails {
    pub fn new() -> Self {
        Self::with_root(cache_root(), load_thumbnailers())
    }

    pub fn with_root(root: PathBuf, thumbnailers: HashMap<String, Thumbnailer>) -> Self {
        Self { root, thumbnailers, max_native_bytes: 200 * 1024 * 1024, timeout: Duration::from_secs(20) }
    }

    pub fn can_thumbnail(&self, mime: &str) -> bool {
        decodes_natively(mime) || self.thumbnailers.contains_key(mime)
    }

    /// An existing valid thumbnail (ours or another app's), at `size` or larger.
    pub fn lookup(&self, path: &Path, size: Size) -> Option<PathBuf> {
        let uri = uri_for(path);
        let mt = mtime(path)?;
        for s in [Size::Normal, Size::Large, Size::XLarge, Size::XXLarge] {
            if s.pixels() < size.pixels() {
                continue;
            }
            let p = cache_path(&self.root, &uri, s);
            if is_valid(&p, &uri, mt) {
                return Some(p);
            }
        }
        None
    }

    /// Get or make a thumbnail. Blocking: call from a worker thread.
    pub fn get(&self, path: &Path, mime: &str, size: Size) -> Outcome {
        if let Some(p) = self.lookup(path, size) {
            return Outcome::Ready(p);
        }
        let uri = uri_for(path);
        let Some(mt) = mtime(path) else { return Outcome::Failed };
        let fail = fail_path(&self.root, &uri);
        if is_valid(&fail, &uri, mt) {
            return Outcome::Failed;
        }
        // Never thumbnail inside the thumbnail cache itself.
        if path.starts_with(&self.root) {
            return Outcome::Unsupported;
        }
        let target = cache_path(&self.root, &uri, size);
        let made = if decodes_natively(mime) {
            self.native(path, size)
        } else if let Some(t) = self.thumbnailers.get(mime) {
            self.external(t, path, &uri, size)
        } else {
            return Outcome::Unsupported;
        };
        match made.and_then(|img| write_thumbnail(&img, &target, &uri, mt, fs::metadata(path).map(|m| m.len()).ok())) {
            Ok(()) => Outcome::Ready(target),
            Err(_) => {
                let _ = write_thumbnail(&image::RgbaImage::new(1, 1).into(), &fail, &uri, mt, None);
                Outcome::Failed
            }
        }
    }

    /// Store an image rendered elsewhere (e.g. a PDF page drawn by the UI's
    /// Poppler fallback) as a valid shared-cache thumbnail, replacing any
    /// failure marker.
    pub fn store_png(&self, path: &Path, size: Size, png_bytes: &[u8]) -> io::Result<PathBuf> {
        let uri = uri_for(path);
        let mt = mtime(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let img = image::load_from_memory(png_bytes).map_err(io::Error::other)?;
        let target = cache_path(&self.root, &uri, size);
        write_thumbnail(&scale(img, size.pixels()), &target, &uri, mt, fs::metadata(path).map(|m| m.len()).ok())?;
        let _ = fs::remove_file(fail_path(&self.root, &uri));
        Ok(target)
    }

    /// Like [`Thumbnails::store_png`], from raw RGBA pixels.
    pub fn store_rgba(&self, path: &Path, size: Size, width: u32, height: u32, rgba: Vec<u8>) -> io::Result<PathBuf> {
        let img = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| io::Error::other("bad buffer"))?;
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(io::Error::other)?;
        self.store_png(path, size, &png)
    }

    fn native(&self, path: &Path, size: Size) -> io::Result<image::DynamicImage> {
        let len = fs::metadata(path)?.len();
        if len > self.max_native_bytes {
            return Err(io::Error::other("too large"));
        }
        let img = image::ImageReader::open(path)?.with_guessed_format()?.decode().map_err(io::Error::other)?;
        Ok(scale(img, size.pixels()))
    }

    fn external(&self, t: &Thumbnailer, path: &Path, uri: &str, size: Size) -> io::Result<image::DynamicImage> {
        let tmp = tempfile_in(&std::env::temp_dir(), "kivo-thumb", ".png")?;
        let argv = t.argv(path, uri, &tmp, size.pixels());
        let (bin, args) = argv.split_first().ok_or_else(|| io::Error::other("empty Exec"))?;
        let mut child = Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if start.elapsed() > self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&tmp);
                return Err(io::Error::new(io::ErrorKind::TimedOut, "thumbnailer timed out"));
            }
            std::thread::sleep(Duration::from_millis(15));
        };
        let result = if status.success() {
            image::open(&tmp).map(|i| scale(i, size.pixels())).map_err(io::Error::other)
        } else {
            Err(io::Error::other("thumbnailer failed"))
        };
        let _ = fs::remove_file(&tmp);
        result
    }
}

impl Default for Thumbnails {
    fn default() -> Self {
        Self::new()
    }
}

fn scale(img: image::DynamicImage, max: u32) -> image::DynamicImage {
    if img.width() <= max && img.height() <= max {
        img
    } else {
        img.thumbnail(max, max)
    }
}

fn tempfile_in(dir: &Path, prefix: &str, suffix: &str) -> io::Result<PathBuf> {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let p = dir.join(format!("{prefix}-{}-{n}{suffix}", std::process::id()));
    Ok(p)
}

/// Write a spec-compliant thumbnail atomically.
pub fn write_thumbnail(img: &image::DynamicImage, target: &Path, uri: &str, mt: i64, size: Option<u64>) -> io::Result<()> {
    let dir = target.parent().ok_or_else(|| io::Error::other("no parent"))?;
    fs::create_dir_all(dir)?;
    let _ = fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let tmp = tempfile_in(dir, ".kivo", ".png.tmp")?;
    let rgba = img.to_rgba8();
    {
        let f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        let mut enc = png::Encoder::new(BufWriter::new(f), rgba.width(), rgba.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.add_text_chunk("Thumb::URI".into(), uri.into()).map_err(io::Error::other)?;
        enc.add_text_chunk("Thumb::MTime".into(), mt.to_string()).map_err(io::Error::other)?;
        if let Some(s) = size {
            enc.add_text_chunk("Thumb::Size".into(), s.to_string()).map_err(io::Error::other)?;
        }
        enc.add_text_chunk("Software".into(), concat!("Kivo ", env!("CARGO_PKG_VERSION")).into()).map_err(io::Error::other)?;
        let mut w = enc.write_header().map_err(io::Error::other)?;
        w.write_image_data(rgba.as_raw()).map_err(io::Error::other)?;
        w.finish().map_err(io::Error::other)?;
    }
    fs::rename(&tmp, target).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_paths() {
        // Example from the spec.
        assert_eq!(hash("file:///home/jens/photos/me.png"), "c6ee772d9e49320e97ec29a7eb5b1697");
        assert_eq!(uri_for(Path::new("/home/u/My Pics/a#b.jpg")), "file:///home/u/My%20Pics/a%23b.jpg");
        let p = cache_path(Path::new("/c"), "file:///x", Size::Large);
        assert!(p.starts_with("/c/large"));
        assert_eq!(Size::for_pixels(64), Size::Normal);
        assert_eq!(Size::for_pixels(200), Size::Large);
    }

    #[test]
    fn parses_thumbnailer_files() {
        let t = Thumbnailer::parse(
            "[Thumbnailer Entry]\nTryExec=papers-thumbnailer\nExec=papers-thumbnailer -s %s %u \"%o\"\nMimeType=application/pdf;application/x-bzpdf;\n",
        )
        .unwrap();
        assert_eq!(t.mime_types, ["application/pdf", "application/x-bzpdf"]);
        let argv = t.argv(Path::new("/a b.pdf"), "file:///a%20b.pdf", Path::new("/tmp/o.png"), 256);
        assert_eq!(argv, ["papers-thumbnailer", "-s", "256", "file:///a%20b.pdf", "/tmp/o.png"]);
        assert!(Thumbnailer::parse("[Other]\nExec=x").is_none());
    }

    #[test]
    fn makes_validates_and_reuses_thumbnails() {
        let d = tempfile::tempdir().unwrap();
        let img_path = d.path().join("pic.png");
        image::RgbaImage::from_pixel(800, 400, image::Rgba([200, 30, 30, 255])).save(&img_path).unwrap();
        let t = Thumbnails::with_root(d.path().join("cache"), HashMap::new());
        let Outcome::Ready(p) = t.get(&img_path, "image/png", Size::Normal) else { panic!("no thumbnail") };
        let thumb = image::open(&p).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (128, 64));
        let uri = uri_for(&img_path);
        let text = png_text(&p).unwrap();
        assert_eq!(text["Thumb::URI"], uri);
        assert!(is_valid(&p, &uri, mtime(&img_path).unwrap()));
        let mode = fs::metadata(&p).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
        // A larger request isn't served by a smaller thumbnail.
        assert!(t.lookup(&img_path, Size::Large).is_none());
        // Touching the file invalidates it.
        let f = fs::File::options().write(true).open(&img_path).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000)).unwrap();
        assert!(t.lookup(&img_path, Size::Normal).is_none());
    }

    #[test]
    fn stores_rendered_images() {
        let d = tempfile::tempdir().unwrap();
        let doc = d.path().join("doc.pdf");
        fs::write(&doc, b"%PDF").unwrap();
        let t = Thumbnails::with_root(d.path().join("cache"), HashMap::new());
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(600, 800, image::Rgba([255, 255, 255, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let p = t.store_png(&doc, Size::Large, &png).unwrap();
        assert_eq!(image::open(&p).unwrap().height(), 256);
        assert_eq!(t.lookup(&doc, Size::Large), Some(p));
    }

    #[test]
    fn failures_are_remembered() {
        let d = tempfile::tempdir().unwrap();
        let bad = d.path().join("broken.png");
        fs::write(&bad, b"not a png").unwrap();
        let t = Thumbnails::with_root(d.path().join("cache"), HashMap::new());
        assert_eq!(t.get(&bad, "image/png", Size::Normal), Outcome::Failed);
        let fail = fail_path(&d.path().join("cache"), &uri_for(&bad));
        assert!(fail.exists());
        assert_eq!(t.get(&bad, "image/png", Size::Normal), Outcome::Failed);
        assert_eq!(t.get(&bad, "application/x-unknown", Size::Normal), Outcome::Failed);
        let other = d.path().join("x.bin");
        fs::write(&other, b"x").unwrap();
        assert_eq!(t.get(&other, "application/x-unknown", Size::Normal), Outcome::Unsupported);
    }

    #[test]
    fn external_thumbnailer_runs_with_timeout() {
        let d = tempfile::tempdir().unwrap();
        // A fake thumbnailer: copies a PNG we prepared to %o.
        let src = d.path().join("ready.png");
        image::RgbaImage::from_pixel(300, 300, image::Rgba([0, 0, 255, 255])).save(&src).unwrap();
        let script = d.path().join("fake-thumb.sh");
        fs::write(&script, format!("#!/bin/sh\ncp '{}' \"$2\"\n", src.display())).unwrap();
        fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let t = Thumbnailer { exec: format!("{} %u %o", script.display()), try_exec: None, mime_types: vec!["video/x-test".into()] };
        let mut map = HashMap::new();
        map.insert("video/x-test".to_string(), t);
        let input = d.path().join("clip.vid");
        fs::write(&input, b"video").unwrap();
        let mut th = Thumbnails::with_root(d.path().join("cache"), map.clone());
        let Outcome::Ready(p) = th.get(&input, "video/x-test", Size::Large) else { panic!() };
        assert_eq!(image::open(&p).unwrap().width(), 256);
        assert!(is_valid(&p, &uri_for(&input), mtime(&input).unwrap()));

        // A hanging thumbnailer is killed.
        let hang = d.path().join("hang.sh");
        fs::write(&hang, "#!/bin/sh\nsleep 30\n").unwrap();
        fs::set_permissions(&hang, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        map.insert("video/x-hang".into(), Thumbnailer { exec: format!("{} %o", hang.display()), try_exec: None, mime_types: vec![] });
        th = Thumbnails::with_root(d.path().join("cache2"), map);
        th.timeout = Duration::from_millis(300);
        let start = Instant::now();
        assert_eq!(th.get(&input, "video/x-hang", Size::Normal), Outcome::Failed);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
