// SPDX-License-Identifier: GPL-3.0-or-later
//! Compression ("Compress…"): ZIP, tar.gz, tar.xz, tar.zst and 7z, written
//! to a hidden temporary file and renamed into place, never overwriting.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::ArchiveError;
use crate::names;
use crate::ops::{self, Cancel, Observer, OpError, Progress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressFormat {
    Zip,
    TarGz,
    TarXz,
    TarZst,
    SevenZ,
}

impl CompressFormat {
    pub const ALL: [CompressFormat; 5] =
        [CompressFormat::Zip, CompressFormat::TarGz, CompressFormat::TarXz, CompressFormat::TarZst, CompressFormat::SevenZ];

    pub fn extension(self) -> &'static str {
        match self {
            CompressFormat::Zip => ".zip",
            CompressFormat::TarGz => ".tar.gz",
            CompressFormat::TarXz => ".tar.xz",
            CompressFormat::TarZst => ".tar.zst",
            CompressFormat::SevenZ => ".7z",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CompressFormat::Zip => "ZIP (.zip) — opens everywhere",
            CompressFormat::TarGz => "Tar + gzip (.tar.gz) — common on Linux",
            CompressFormat::TarXz => "Tar + xz (.tar.xz) — smallest, slower",
            CompressFormat::TarZst => "Tar + Zstandard (.tar.zst) — fast",
            CompressFormat::SevenZ => "7-Zip (.7z) — small, needs 7-Zip on Windows",
        }
    }

    pub fn supports_password(self) -> bool {
        matches!(self, CompressFormat::Zip | CompressFormat::SevenZ)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Link,
}

struct Item {
    abs: PathBuf,
    rel: String,
    kind: Kind,
    meta: fs::Metadata,
}

fn collect(src: &Path, rel: String, out: &mut Vec<Item>, cancel: &Cancel) -> Result<(), ArchiveError> {
    if cancel.is_cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    let meta = fs::symlink_metadata(src).map_err(|e| ArchiveError::Io(OpError::io(&e, src, "read")))?;
    let kind = if meta.file_type().is_symlink() {
        Kind::Link
    } else if meta.is_dir() {
        Kind::Dir
    } else if meta.is_file() {
        Kind::File
    } else {
        return Ok(()); // devices, sockets, pipes: skipped
    };
    out.push(Item { abs: src.to_path_buf(), rel: rel.clone(), kind, meta });
    if kind == Kind::Dir {
        let mut children: Vec<_> = fs::read_dir(src)
            .map_err(|e| ArchiveError::Io(OpError::io(&e, src, "read")))?
            .flatten()
            .map(|e| e.path())
            .collect();
        children.sort();
        for c in children {
            let name = c.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            collect(&c, format!("{rel}/{name}"), out, cancel)?;
        }
    }
    Ok(())
}

/// Counts bytes read for progress and stops when cancelled.
struct Tracked<'a, R> {
    inner: R,
    progress: &'a mut Progress,
    obs: &'a mut dyn Observer,
    cancel: &'a Cancel,
    last: std::time::Instant,
}

impl<R: Read> Read for Tracked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.progress.bytes_done += n as u64;
        if self.last.elapsed() >= std::time::Duration::from_millis(80) {
            self.last = std::time::Instant::now();
            self.obs.progress(self.progress);
        }
        Ok(n)
    }
}

/// Compress `sources` into `dest_dir/name` (name includes the extension).
/// Returns the archive's path. Never overwrites: the name is numbered if taken.
pub fn compress(
    sources: &[PathBuf],
    dest_dir: &Path,
    name: &str,
    format: CompressFormat,
    password: Option<&str>,
    obs: &mut dyn Observer,
    cancel: &Cancel,
) -> Result<PathBuf, ArchiveError> {
    names::validate(name).map_err(|m| ArchiveError::Io(OpError::new(dest_dir, m, "Choose a different name.")))?;
    let mut items = Vec::new();
    for s in sources {
        let top = s.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
        collect(s, top, &mut items, cancel)?;
    }
    let mut progress = Progress {
        files_total: items.len() as u64,
        bytes_total: items.iter().filter(|i| i.kind == Kind::File).map(|i| i.meta.len()).sum(),
        ..Default::default()
    };
    obs.progress(&progress);

    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let staging = dest_dir.join(format!(".strata-partial-{}-{n}-{name}", std::process::id()));
    let journal = ops::journal::file();
    ops::journal::add(&journal, dest_dir);

    let result = (|| {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|e| ArchiveError::Io(OpError::io(&e, dest_dir, "create an archive in")))?;
        let w = BufWriter::new(file);
        match format {
            CompressFormat::Zip => write_zip(w, &items, password, &mut progress, obs, cancel)?,
            CompressFormat::TarGz => {
                let enc = flate2::write::GzEncoder::new(w, flate2::Compression::default());
                write_tar(enc, &items, &mut progress, obs, cancel)?.finish().map_err(io_err)?;
            }
            CompressFormat::TarXz => {
                let enc = liblzma::write::XzEncoder::new(w, 6);
                write_tar(enc, &items, &mut progress, obs, cancel)?.finish().map_err(io_err)?;
            }
            CompressFormat::TarZst => {
                let enc = zstd::stream::write::Encoder::new(w, 3).map_err(io_err)?;
                write_tar(enc, &items, &mut progress, obs, cancel)?.finish().map_err(io_err)?;
            }
            CompressFormat::SevenZ => write_7z(w, &items, password, &mut progress, obs, cancel)?,
        }
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        for _ in 0..100 {
            let final_path = names::unique_path(dest_dir, name);
            match ops::rename_noreplace(&staging, &final_path) {
                Ok(()) => return Ok(final_path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(ArchiveError::Io(OpError::io(&e, &final_path, "create"))),
            }
        }
        Err(ArchiveError::Io(OpError::new(dest_dir, "Couldn't find a free name.", "Choose a different name.")))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    ops::journal::remove(&journal, dest_dir);
    progress.files_done = progress.files_total;
    obs.progress(&progress);
    result
}

fn io_err(e: io::Error) -> ArchiveError {
    if e.to_string() == "cancelled" {
        ArchiveError::Cancelled
    } else if e.kind() == io::ErrorKind::StorageFull {
        ArchiveError::NoSpace(e.to_string())
    } else {
        ArchiveError::Io(OpError { path: PathBuf::new(), message: "Couldn't write the archive.".into(), hint: e.to_string() })
    }
}

fn write_tar<W: Write>(
    w: W,
    items: &[Item],
    progress: &mut Progress,
    obs: &mut dyn Observer,
    cancel: &Cancel,
) -> Result<W, ArchiveError> {
    let mut b = tar::Builder::new(w);
    b.follow_symlinks(false);
    for it in items {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        progress.current = it.rel.clone();
        match it.kind {
            Kind::Dir | Kind::Link => b.append_path_with_name(&it.abs, &it.rel).map_err(io_err)?,
            Kind::File => {
                let mut header = tar::Header::new_gnu();
                header.set_metadata(&it.meta);
                let f = File::open(&it.abs).map_err(|e| ArchiveError::Io(OpError::io(&e, &it.abs, "read")))?;
                let r = Tracked { inner: f, progress, obs, cancel, last: std::time::Instant::now() };
                b.append_data(&mut header, &it.rel, r).map_err(io_err)?;
            }
        }
        progress.files_done += 1;
    }
    b.into_inner().map_err(io_err)
}

fn zip_time(secs: i64) -> Option<zip::DateTime> {
    use chrono::{Datelike, TimeZone, Timelike};
    let t = chrono::Local.timestamp_opt(secs, 0).single()?;
    zip::DateTime::from_date_and_time(t.year() as u16, t.month() as u8, t.day() as u8, t.hour() as u8, t.minute() as u8, t.second() as u8).ok()
}

fn write_zip<W: Write + io::Seek>(
    w: W,
    items: &[Item],
    password: Option<&str>,
    progress: &mut Progress,
    obs: &mut dyn Observer,
    cancel: &Cancel,
) -> Result<(), ArchiveError> {
    use zip::write::SimpleFileOptions;
    let zerr = |e: zip::result::ZipError| match e {
        zip::result::ZipError::Io(e) => io_err(e),
        other => ArchiveError::Io(OpError { path: PathBuf::new(), message: "Couldn't write the archive.".into(), hint: other.to_string() }),
    };
    let mut z = zip::ZipWriter::new(w);
    for it in items {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        progress.current = it.rel.clone();
        let mut opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(it.meta.mode() & 0o777)
            .large_file(it.meta.len() >= 0xFFFF_FFFF);
        if let Some(t) = zip_time(it.meta.mtime()) {
            opts = opts.last_modified_time(t);
        }
        match it.kind {
            Kind::Dir => z.add_directory(format!("{}/", it.rel), opts).map_err(zerr)?,
            Kind::Link => {
                let target = fs::read_link(&it.abs).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
                z.add_symlink(&it.rel, target, opts).map_err(zerr)?;
            }
            Kind::File => {
                match password {
                    Some(p) => z.start_file(&it.rel, opts.with_aes_encryption(zip::AesMode::Aes256, p)).map_err(zerr)?,
                    None => z.start_file(&it.rel, opts).map_err(zerr)?,
                }
                let f = File::open(&it.abs).map_err(|e| ArchiveError::Io(OpError::io(&e, &it.abs, "read")))?;
                let mut r = Tracked { inner: f, progress, obs, cancel, last: std::time::Instant::now() };
                io::copy(&mut r, &mut z).map_err(io_err)?;
            }
        }
        progress.files_done += 1;
    }
    z.finish().map_err(zerr)?.flush().map_err(io_err)
}

fn write_7z<W: Write + io::Seek>(
    w: W,
    items: &[Item],
    password: Option<&str>,
    progress: &mut Progress,
    obs: &mut dyn Observer,
    cancel: &Cancel,
) -> Result<(), ArchiveError> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, Password};
    let serr = |e: sevenz_rust2::Error| match e {
        sevenz_rust2::Error::Io(e, _) => io_err(e),
        other => ArchiveError::Io(OpError { path: PathBuf::new(), message: "Couldn't write the archive.".into(), hint: other.to_string() }),
    };
    let mut z = ArchiveWriter::new(w).map_err(serr)?;
    if let Some(p) = password.filter(|p| !p.is_empty()) {
        z.set_content_methods(vec![
            sevenz_rust2::encoder_options::AesEncoderOptions::new(Password::new(p)).into(),
            sevenz_rust2::EncoderMethod::LZMA2.into(),
        ]);
        z.set_encrypt_header(true);
    }
    for it in items {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        progress.current = it.rel.clone();
        match it.kind {
            Kind::Dir => {
                z.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory(&it.rel), None).map_err(serr)?;
            }
            // 7z has no portable symlinks; the linked file's contents are stored.
            Kind::File | Kind::Link => {
                if it.kind == Kind::Link && !it.abs.is_file() {
                    continue;
                }
                let entry = ArchiveEntry::from_path(&it.abs, it.rel.clone());
                let f = File::open(&it.abs).map_err(|e| ArchiveError::Io(OpError::io(&e, &it.abs, "read")))?;
                let r = Tracked { inner: f, progress, obs, cancel, last: std::time::Instant::now() };
                z.push_archive_entry(entry, Some(r)).map_err(serr)?;
            }
        }
        progress.files_done += 1;
    }
    z.finish().map_err(io_err)?.flush().map_err(io_err)
}
