// SPDX-License-Identifier: GPL-3.0-or-later
//! Archives: detection by content, safe extraction, and compression.
//!
//! Extraction rules (macOS-style double-click):
//! - Extract next to the archive. One top-level item is kept as is; several
//!   go into a folder named after the archive. Never overwrite: names are
//!   numbered ("Photos (2)").
//! - Everything is written into a hidden staging folder first and moved into
//!   place at the end, so a cancel or failure leaves nothing behind.
//! - Entry paths are sanitized: absolute paths are made relative, and any
//!   path that climbs out with ".." aborts the whole extraction.
//! - Symlinks are created last (so nothing is ever written through one) and
//!   removed again if they resolve outside the extracted folder.
//! - Archive bombs are stopped by limits on total size, entry count and
//!   compression ratio, and by watching free space.
//! - setuid/setgid bits are dropped; extracted items are always writable by
//!   their owner so they can be deleted.

mod list;
mod read;
mod write;

use std::fs::{self, File, FileTimes};
use std::io::{self, Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

pub use list::{ListEntry, Listing, list};
pub use write::{CompressFormat, compress};

use crate::names;
use crate::ops::{self, Cancel, Observer, OpError, Progress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
    Gz,
    Bz2,
    Xz,
    Zst,
    SevenZ,
    Rar,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "ZIP archive",
            Format::Tar => "Tar archive",
            Format::TarGz => "Tar archive (gzip)",
            Format::TarBz2 => "Tar archive (bzip2)",
            Format::TarXz => "Tar archive (xz)",
            Format::TarZst => "Tar archive (Zstandard)",
            Format::Gz => "Gzip-compressed file",
            Format::Bz2 => "Bzip2-compressed file",
            Format::Xz => "XZ-compressed file",
            Format::Zst => "Zstandard-compressed file",
            Format::SevenZ => "7-Zip archive",
            Format::Rar => "RAR archive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Gz,
    Bz2,
    Xz,
    Zst,
}

/// Identify an archive by its first bytes, not its name.
pub fn detect(path: &Path) -> io::Result<Option<Format>> {
    let mut f = File::open(path)?;
    let mut head = [0u8; 512];
    let n = read_up_to(&mut f, &mut head)?;
    let h = &head[..n];
    let stream = if h.starts_with(&[0x1f, 0x8b]) {
        Some(Stream::Gz)
    } else if h.starts_with(b"BZh") && n > 3 && h[3].is_ascii_digit() {
        Some(Stream::Bz2)
    } else if h.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0]) {
        Some(Stream::Xz)
    } else if h.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        Some(Stream::Zst)
    } else {
        None
    };
    if let Some(s) = stream {
        // Compressed: is there a tar inside?
        let mut dec = read::decoder(s, File::open(path)?)?;
        let mut inner = [0u8; 512];
        let m = read_up_to(&mut dec, &mut inner).unwrap_or(0);
        let tar = m == 512 && looks_like_tar(&inner);
        return Ok(Some(match (s, tar) {
            (Stream::Gz, true) => Format::TarGz,
            (Stream::Bz2, true) => Format::TarBz2,
            (Stream::Xz, true) => Format::TarXz,
            (Stream::Zst, true) => Format::TarZst,
            (Stream::Gz, false) => Format::Gz,
            (Stream::Bz2, false) => Format::Bz2,
            (Stream::Xz, false) => Format::Xz,
            (Stream::Zst, false) => Format::Zst,
        }));
    }
    if h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06") || h.starts_with(b"PK\x07\x08") {
        return Ok(Some(Format::Zip));
    }
    if h.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return Ok(Some(Format::SevenZ));
    }
    if h.starts_with(b"Rar!\x1a\x07\x00") || h.starts_with(b"Rar!\x1a\x07\x01\x00") {
        return Ok(Some(Format::Rar));
    }
    if n == 512 && looks_like_tar(&head) {
        return Ok(Some(Format::Tar));
    }
    Ok(None)
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// POSIX ustar magic, or a valid old-style (v7) header checksum.
fn looks_like_tar(h: &[u8]) -> bool {
    if h.len() < 512 {
        return false;
    }
    if &h[257..262] == b"ustar" {
        return true;
    }
    let stored = std::str::from_utf8(&h[148..156])
        .ok()
        .map(|s| s.trim_matches(|c: char| c == '\0' || c == ' '))
        .and_then(|s| u32::from_str_radix(s, 8).ok());
    let Some(stored) = stored else { return false };
    let sum: u32 = h.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as u32 }).sum();
    stored == sum && h[0] != 0
}

// ------------------------------------------------------------------ errors

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    NeedPassword,
    WrongPassword,
    /// Path traversal: nothing was extracted.
    Unsafe(String),
    /// Would exceed a size, count or ratio limit: nothing was extracted.
    TooBig(String),
    NoSpace(String),
    Corrupt(String),
    Unsupported(String),
    Empty,
    Cancelled,
    Io(OpError),
}

impl ArchiveError {
    pub fn message(&self, archive: &str) -> String {
        match self {
            ArchiveError::NeedPassword => format!("“{archive}” is protected with a password."),
            ArchiveError::WrongPassword => "That password isn't right.".into(),
            ArchiveError::Unsafe(_) => format!("“{archive}” tries to put files outside its own folder."),
            ArchiveError::TooBig(_) => format!("“{archive}” would expand to an unreasonable size."),
            ArchiveError::NoSpace(_) => "There isn't enough free space to extract this archive.".into(),
            ArchiveError::Corrupt(_) => format!("“{archive}” is damaged or incomplete."),
            ArchiveError::Unsupported(_) => format!("“{archive}” can't be opened."),
            ArchiveError::Empty => format!("“{archive}” is empty."),
            ArchiveError::Cancelled => "Extraction was cancelled.".into(),
            ArchiveError::Io(e) => e.message.clone(),
        }
    }

    pub fn hint(&self) -> String {
        match self {
            ArchiveError::NeedPassword | ArchiveError::WrongPassword => "Enter the password to extract it.".into(),
            ArchiveError::Unsafe(d) => format!("That's a sign of a malicious archive, so nothing was extracted. ({d})"),
            ArchiveError::TooBig(d) => format!("It may be an “archive bomb”, so nothing was extracted. ({d})"),
            ArchiveError::NoSpace(d) => format!("Free up some space, then try again. ({d})"),
            ArchiveError::Corrupt(d) => format!("Try downloading it again. ({d})"),
            ArchiveError::Unsupported(d) => d.clone(),
            ArchiveError::Empty => "There was nothing to extract.".into(),
            ArchiveError::Cancelled => "Nothing was left behind.".into(),
            ArchiveError::Io(e) => e.hint.clone(),
        }
    }
}

// ------------------------------------------------------------------ limits

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Hard cap on bytes written.
    pub max_bytes: u64,
    pub max_entries: u64,
    /// Written bytes may not exceed `max_ratio` × archive size …
    pub max_ratio: u64,
    /// … once more than this much has been written.
    pub ratio_floor: u64,
    /// Keep this much free on the drive.
    pub keep_free: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 200 * 1024 * 1024 * 1024,
            max_entries: 2_000_000,
            max_ratio: 1000,
            ratio_floor: 1024 * 1024 * 1024,
            keep_free: 256 * 1024 * 1024,
        }
    }
}

// -------------------------------------------------------------------- sink

/// Where extracted entries go. Every write is checked here.
pub(crate) struct Sink<'a> {
    root: PathBuf,
    limits: Limits,
    archive_size: u64,
    written: u64,
    entries: u64,
    links: Vec<(PathBuf, PathBuf)>,
    dirs: Vec<(PathBuf, u32, Option<i64>)>,
    obs: &'a mut dyn Observer,
    cancel: &'a Cancel,
    progress: Progress,
    next_space_check: u64,
    buf: Vec<u8>,
    /// Bytes of the archive consumed so far, set by the format reader.
    pub(crate) input: Option<std::rc::Rc<std::cell::Cell<u64>>>,
    last_emit: std::time::Instant,
}

pub(crate) struct OpenFile {
    file: File,
    path: PathBuf,
    mode: u32,
    mtime: Option<i64>,
}

impl<'a> Sink<'a> {
    fn new(root: PathBuf, archive_size: u64, limits: Limits, obs: &'a mut dyn Observer, cancel: &'a Cancel) -> Self {
        let progress = Progress { bytes_total: archive_size, ..Default::default() };
        Self {
            root,
            limits,
            archive_size,
            written: 0,
            entries: 0,
            links: Vec::new(),
            dirs: Vec::new(),
            obs,
            cancel,
            progress,
            next_space_check: 0,
            buf: vec![0; 256 * 1024],
            input: None,
            last_emit: std::time::Instant::now(),
        }
    }

    pub(crate) fn check_cancel(&self) -> Result<(), ArchiveError> {
        if self.cancel.is_cancelled() { Err(ArchiveError::Cancelled) } else { Ok(()) }
    }

    /// Report how much of the archive has been read.
    pub(crate) fn input_progress(&mut self, consumed: u64) {
        self.progress.bytes_done = consumed.min(self.archive_size);
        self.obs.progress(&self.progress);
    }

    fn count_entry(&mut self, name: &str) -> Result<(), ArchiveError> {
        self.check_cancel()?;
        self.entries += 1;
        self.progress.files_done = self.entries;
        self.progress.current = name.to_string();
        if self.entries > self.limits.max_entries {
            return Err(ArchiveError::TooBig(format!("more than {} items", self.limits.max_entries)));
        }
        Ok(())
    }

    /// Map an archive entry name to a path inside the staging root.
    pub(crate) fn resolve(&self, name: &str) -> Result<Option<PathBuf>, ArchiveError> {
        let rel = sanitize(name)?;
        if rel.as_os_str().is_empty() {
            return Ok(None);
        }
        Ok(Some(self.root.join(rel)))
    }

    fn ensure_parent(&self, path: &Path) -> Result<(), ArchiveError> {
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent) {
                return Err(if e.kind() == io::ErrorKind::NotADirectory || e.kind() == io::ErrorKind::AlreadyExists {
                    ArchiveError::Corrupt(format!("“{}” is both a file and a folder", self.rel(parent)))
                } else {
                    ArchiveError::Io(OpError::io(&e, parent, "create"))
                });
            }
        Ok(())
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).display().to_string()
    }

    pub(crate) fn dir(&mut self, name: &str, mode: Option<u32>, mtime: Option<i64>) -> Result<(), ArchiveError> {
        self.count_entry(name)?;
        let Some(path) = self.resolve(name)? else { return Ok(()) };
        match fs::create_dir_all(&path) {
            Ok(()) => {}
            Err(e) => return Err(ArchiveError::Io(OpError::io(&e, &path, "create"))),
        }
        self.dirs.push((path, mode.unwrap_or(0o755), mtime));
        Ok(())
    }

    pub(crate) fn open_file(&mut self, name: &str, mode: Option<u32>, mtime: Option<i64>) -> Result<Option<OpenFile>, ArchiveError> {
        self.count_entry(name)?;
        let Some(path) = self.resolve(name)? else { return Ok(None) };
        self.ensure_parent(&path)?;
        if let Ok(m) = fs::symlink_metadata(&path) {
            if m.is_dir() {
                return Err(ArchiveError::Corrupt(format!("“{}” is both a file and a folder", self.rel(&path))));
            }
            // A later entry with the same name replaces the earlier one (tar semantics).
            let _ = fs::remove_file(&path);
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| ArchiveError::Io(OpError::io(&e, &path, "create")))?;
        Ok(Some(OpenFile { file, path, mode: mode.unwrap_or(0o644), mtime }))
    }

    pub(crate) fn write(&mut self, f: &mut OpenFile, data: &[u8]) -> Result<(), ArchiveError> {
        self.check_cancel()?;
        self.written += data.len() as u64;
        let l = self.limits;
        if self.written > l.max_bytes {
            return Err(ArchiveError::TooBig(format!("more than {}", crate::format::size(l.max_bytes))));
        }
        if self.written > l.ratio_floor && self.written / self.archive_size.max(1) > l.max_ratio {
            return Err(ArchiveError::TooBig(format!(
                "over {}× larger than the archive",
                l.max_ratio
            )));
        }
        if self.written >= self.next_space_check {
            self.next_space_check = self.written + 64 * 1024 * 1024;
            if let Some(free) = ops::free_space(&self.root)
                && free < l.keep_free + data.len() as u64 {
                    return Err(ArchiveError::NoSpace(format!("{} written so far", crate::format::size(self.written))));
                }
        }
        if self.last_emit.elapsed() >= std::time::Duration::from_millis(80) {
            self.last_emit = std::time::Instant::now();
            if let Some(c) = self.input.clone() {
                self.input_progress(c.get());
            }
        }
        f.file.write_all(data).map_err(|e| {
            if e.kind() == io::ErrorKind::StorageFull {
                ArchiveError::NoSpace(crate::format::size(self.written))
            } else {
                ArchiveError::Io(OpError::io(&e, &f.path, "write"))
            }
        })
    }

    pub(crate) fn close(&mut self, f: OpenFile) -> Result<(), ArchiveError> {
        let _ = fs::set_permissions(&f.path, fs::Permissions::from_mode(safe_mode(f.mode, false)));
        if let Some(t) = f.mtime.and_then(to_system_time) {
            let _ = f.file.set_times(FileTimes::new().set_modified(t));
        }
        Ok(())
    }

    /// Write a whole entry from a reader.
    pub(crate) fn file_from(&mut self, name: &str, mode: Option<u32>, mtime: Option<i64>, r: &mut dyn Read) -> Result<(), ArchiveError> {
        let Some(mut f) = self.open_file(name, mode, mtime)? else {
            // A nameless entry (e.g. "./"): drain it.
            io::copy(r, &mut io::sink()).map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            return Ok(());
        };
        let mut buf = std::mem::take(&mut self.buf);
        let res = (|| loop {
            let n = match r.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(read_error(e)),
            };
            self.write(&mut f, &buf[..n])?;
        })();
        self.buf = buf;
        res?;
        self.close(f)
    }

    /// Symlinks are recorded now and created after everything else.
    pub(crate) fn symlink(&mut self, name: &str, target: &str) -> Result<(), ArchiveError> {
        self.count_entry(name)?;
        let Some(path) = self.resolve(name)? else { return Ok(()) };
        self.links.push((path, PathBuf::from(target)));
        Ok(())
    }

    /// Tar hard links: copy the already-extracted target's contents.
    pub(crate) fn hardlink(&mut self, name: &str, target: &str) -> Result<(), ArchiveError> {
        let Some(src) = self.resolve(target)? else { return Ok(()) };
        if !fs::symlink_metadata(&src).map(|m| m.is_file()).unwrap_or(false) {
            // Target missing or not a regular file: skip quietly.
            return Ok(());
        }
        let mut input = File::open(&src).map_err(|e| ArchiveError::Io(OpError::io(&e, &src, "read")))?;
        let mode = fs::metadata(&src).ok().map(|m| m.permissions().mode());
        self.file_from(name, mode, None, &mut input)
    }

    /// Create deferred links, drop unsafe ones, apply folder metadata.
    /// Returns the number of links left out.
    fn finish(&mut self) -> Result<usize, ArchiveError> {
        self.check_cancel()?;
        let links = std::mem::take(&mut self.links);
        // Links must stay inside what ends up next to the archive: when the
        // archive holds a single folder, that folder is moved out on its own,
        // so it — not the staging folder — is the boundary.
        let mut top: Vec<PathBuf> = fs::read_dir(&self.root)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        for (p, _) in &links {
            if p.parent() == Some(self.root.as_path()) && !top.contains(p) {
                top.push(p.clone());
            }
        }
        let boundary = match top.as_slice() {
            [only] if fs::symlink_metadata(only).map(|m| m.is_dir()).unwrap_or(false) => only.clone(),
            _ => self.root.clone(),
        };
        let canon_root = fs::canonicalize(&boundary).map_err(|e| ArchiveError::Io(OpError::io(&e, &boundary, "open")))?;
        let mut unsafe_links = 0;
        for (path, target) in &links {
            if target.is_absolute() || !lexically_inside(path.parent().unwrap_or(&self.root), target, &boundary) {
                unsafe_links += 1;
                continue;
            }
            self.ensure_parent(path)?;
            if fs::symlink_metadata(path).is_ok() {
                continue; // duplicate entry
            }
            if symlink(target, path).is_err() {
                unsafe_links += 1;
            }
        }
        // Second pass: resolve each link for real (chains through other links
        // can escape even when every target looks fine on its own).
        for (path, _) in &links {
            if !fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                continue;
            }
            let escapes = match fs::canonicalize(path) {
                Ok(real) => !real.starts_with(&canon_root),
                Err(_) => false, // dangling inside the folder: harmless
            };
            if escapes {
                let _ = fs::remove_file(path);
                unsafe_links += 1;
            }
        }
        // Folders last, deepest first, so their times stick.
        let mut dirs = std::mem::take(&mut self.dirs);
        dirs.sort_by_key(|(p, _, _)| std::cmp::Reverse(p.components().count()));
        for (p, mode, mtime) in dirs {
            let _ = fs::set_permissions(&p, fs::Permissions::from_mode(safe_mode(mode, true)));
            if let (Some(t), Ok(f)) = (mtime.and_then(to_system_time), File::open(&p)) {
                let _ = f.set_times(FileTimes::new().set_modified(t));
            }
        }
        Ok(unsafe_links)
    }
}

fn read_error(e: io::Error) -> ArchiveError {
    let msg = e.to_string();
    if msg.to_lowercase().contains("password") || msg.contains("Invalid checksum") && msg.contains("crypt") {
        return ArchiveError::WrongPassword;
    }
    ArchiveError::Corrupt(msg)
}

/// No setuid/setgid/sticky; owner can always read and write (and enter folders).
fn safe_mode(mode: u32, dir: bool) -> u32 {
    let m = mode & 0o777;
    if dir { m | 0o700 } else { m | 0o600 }
}

fn to_system_time(secs: i64) -> Option<std::time::SystemTime> {
    if secs <= 0 {
        return None;
    }
    std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(secs as u64))
}

/// Turn an archive entry name into a safe relative path.
/// Leading slashes and drive letters are dropped; ".." that would climb out
/// of the extraction folder is an error.
pub fn sanitize(name: &str) -> Result<PathBuf, ArchiveError> {
    if name.contains('\0') {
        return Err(ArchiveError::Unsafe(format!("“{}” contains a null character", name.replace('\0', "␀"))));
    }
    let unified = name.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for (i, part) in unified.split('/').enumerate() {
        match part {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() {
                    return Err(ArchiveError::Unsafe(format!("“{name}” points outside the archive")));
                }
            }
            p if i == 0 && p.len() == 2 && p.ends_with(':') => {} // "C:"
            p => out.push(p),
        }
    }
    Ok(out.iter().collect())
}

/// Does `target`, relative to `dir`, stay inside `root` (lexically)?
fn lexically_inside(dir: &Path, target: &Path, root: &Path) -> bool {
    let mut out = PathBuf::new();
    for c in dir.join(target).components() {
        match c {
            Component::ParentDir => {
                if !out.pop() || !out.starts_with(root) {
                    return false;
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.starts_with(root)
}

// --------------------------------------------------------------- extract

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The folder or single item that now sits next to the archive.
    pub path: PathBuf,
    /// Links that would have pointed outside and were left out.
    pub skipped_links: usize,
}

/// Name for the folder that holds a multi-item archive: "Photos.tar.gz" →
/// "Photos".
pub fn folder_name_for(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Archive".into());
    let (stem, _) = names::split_ext(&name);
    if stem.is_empty() || stem == name { format!("{name} contents") } else { stem.to_string() }
}

/// Output name for a single compressed stream: "notes.txt.gz" → "notes.txt".
fn stream_output_name(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let lower = name.to_lowercase();
    for ext in [".gz", ".gzip", ".bz2", ".bzip2", ".xz", ".zst", ".zstd", ".tgz", ".tbz2", ".txz", ".tzst"] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    format!("{name}.out")
}

/// Extract `archive` next to itself.
pub fn extract(
    archive: &Path,
    password: Option<&str>,
    limits: Limits,
    obs: &mut dyn Observer,
    cancel: &Cancel,
) -> Result<Extracted, ArchiveError> {
    let format = detect(archive)
        .map_err(|e| ArchiveError::Io(OpError::io(&e, archive, "open")))?
        .ok_or_else(|| ArchiveError::Unsupported("It isn't a ZIP, 7-Zip, RAR, tar or compressed file.".into()))?;
    let parent = archive.parent().ok_or_else(|| ArchiveError::Unsupported("No containing folder.".into()))?.to_path_buf();
    let size = fs::metadata(archive).map(|m| m.len()).unwrap_or(0);

    let staging = staging_dir(&parent, archive);
    fs::create_dir(&staging).map_err(|e| ArchiveError::Io(OpError::io(&e, &parent, "extract into")))?;
    let journal = ops::journal::file();
    ops::journal::add(&journal, &parent);

    let result = (|| {
        let mut sink = Sink::new(staging.clone(), size, limits, obs, cancel);
        read::extract_format(format, archive, password, &mut sink, &stream_output_name(archive))?;
        let skipped = sink.finish()?;
        place(&staging, archive, &parent, skipped)
    })();

    if result.is_err() {
        let _ = remove_tree(&staging);
    }
    ops::journal::remove(&journal, &parent);
    result
}

fn staging_dir(parent: &Path, archive: &Path) -> PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    parent.join(format!(".tuck-partial-{}-{n}-{name}", std::process::id()))
}

/// Move the result into place: one top-level item as is, otherwise the
/// staging folder itself becomes "<archive name>". Never overwrites.
fn place(staging: &Path, archive: &Path, parent: &Path, skipped: usize) -> Result<Extracted, ArchiveError> {
    let top: Vec<PathBuf> = fs::read_dir(staging)
        .map_err(|e| ArchiveError::Io(OpError::io(&e, staging, "read")))?
        .flatten()
        .map(|e| e.path())
        .collect();
    if top.is_empty() {
        return Err(ArchiveError::Empty);
    }
    for attempt in 0..100 {
        let final_path = if top.len() == 1 {
            let name = top[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            names::unique_path(parent, &name)
        } else {
            names::unique_path(parent, &folder_name_for(archive))
        };
        let from = if top.len() == 1 { top[0].clone() } else { staging.to_path_buf() };
        match ops::rename_noreplace(&from, &final_path) {
            Ok(()) => {
                if top.len() == 1 {
                    let _ = fs::remove_dir(staging);
                } else {
                    // The staging folder was created 0700; give it normal permissions.
                    let _ = fs::set_permissions(&final_path, fs::Permissions::from_mode(0o755));
                }
                return Ok(Extracted { path: final_path, skipped_links: skipped });
            }
            // Someone took the name in the meantime: pick the next one.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempt < 99 => continue,
            Err(e) => return Err(ArchiveError::Io(OpError::io(&e, &final_path, "create"))),
        }
    }
    unreachable!()
}

fn remove_tree(p: &Path) -> io::Result<()> {
    // Make everything writable first so read-only entries can't block cleanup.
    fn fix(p: &Path) {
        if let Ok(m) = fs::symlink_metadata(p)
            && m.is_dir() {
                let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
                if let Ok(rd) = fs::read_dir(p) {
                    for e in rd.flatten() {
                        fix(&e.path());
                    }
                }
            }
    }
    fix(p);
    fs::remove_dir_all(p)
}

/// Should a double-click extract this file? True when the content is an
/// archive and the name doesn't say it's a document that merely uses ZIP
/// inside (.docx, .odt, .epub, .jar, .apk …).
pub fn is_extractable(path: &Path, format: Option<Format>) -> bool {
    let Some(f) = format else { return false };
    if f != Format::Zip {
        return true;
    }
    let ext = crate::entry::extension_of(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    const ZIP_DOCUMENTS: &[&str] = &[
        "docx", "xlsx", "pptx", "docm", "xlsm", "pptm", "odt", "ods", "odp", "odg", "odf", "ott", "ots", "otp",
        "epub", "jar", "war", "ear", "apk", "aab", "xpi", "crx", "ipa", "appx", "msix", "vsix", "nupkg", "kmz",
        "3mf", "sketch", "xd", "pages", "numbers", "key", "cbz", "whl", "ora", "kra", "xps", "oxps", "vsdx",
    ];
    !ZIP_DOCUMENTS.contains(&ext.as_str())
}

#[cfg(test)]
mod tests;
