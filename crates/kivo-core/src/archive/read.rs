// SPDX-License-Identifier: GPL-3.0-or-later
//! Format readers. Each one walks its archive and hands entries to the Sink,
//! which does all safety checking.

use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::rc::Rc;

use super::{ArchiveError, Format, Sink, Stream};
use crate::ops::OpError;

/// Counts how far into the archive file we've read (for progress).
struct Counting<R> {
    inner: R,
    pos: u64,
    max: Rc<Cell<u64>>,
}

impl<R> Counting<R> {
    fn new(inner: R) -> (Self, Rc<Cell<u64>>) {
        let max = Rc::new(Cell::new(0));
        (Self { inner, pos: 0, max: max.clone() }, max)
    }
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        if self.pos > self.max.get() {
            self.max.set(self.pos);
        }
        Ok(n)
    }
}

impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = self.inner.seek(to)?;
        Ok(self.pos)
    }
}

pub(super) fn decoder(s: Stream, r: impl Read + 'static) -> io::Result<Box<dyn Read>> {
    let r = BufReader::new(r);
    Ok(match s {
        Stream::Gz => Box::new(flate2::read::MultiGzDecoder::new(r)),
        Stream::Bz2 => Box::new(bzip2::read::MultiBzDecoder::new(r)),
        Stream::Xz => Box::new(liblzma::read::XzDecoder::new_multi_decoder(r)),
        Stream::Zst => Box::new(zstd::stream::read::Decoder::with_buffer(r)?),
    })
}

fn open(path: &Path) -> Result<File, ArchiveError> {
    File::open(path).map_err(|e| ArchiveError::Io(OpError::io(&e, path, "open")))
}

pub(super) fn extract_format(
    format: Format,
    path: &Path,
    password: Option<&str>,
    sink: &mut Sink,
    stream_name: &str,
) -> Result<(), ArchiveError> {
    let stream = match format {
        Format::TarGz | Format::Gz => Some(Stream::Gz),
        Format::TarBz2 | Format::Bz2 => Some(Stream::Bz2),
        Format::TarXz | Format::Xz => Some(Stream::Xz),
        Format::TarZst | Format::Zst => Some(Stream::Zst),
        _ => None,
    };
    match format {
        Format::Tar => {
            let (r, count) = Counting::new(BufReader::new(open(path)?));
            tar_entries(r, sink, &count)
        }
        Format::TarGz | Format::TarBz2 | Format::TarXz | Format::TarZst => {
            let (r, count) = Counting::new(open(path)?);
            let dec = decoder(stream.expect("compressed"), r).map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            tar_entries(dec, sink, &count)
        }
        Format::Gz | Format::Bz2 | Format::Xz | Format::Zst => {
            let (r, count) = Counting::new(open(path)?);
            let mut dec = decoder(stream.expect("compressed"), r).map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            let mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
            sink.input = Some(count.clone());
            sink.file_from(stream_name, Some(0o644), mtime, &mut dec)?;
            sink.input_progress(count.get());
            Ok(())
        }
        Format::Zip => zip_entries(path, password, sink),
        Format::SevenZ => sevenz_entries(path, password, sink),
        Format::Rar => rar_entries(path, password, sink),
    }
}

// --------------------------------------------------------------------- tar

fn tar_entries(r: impl Read, sink: &mut Sink, count: &Rc<Cell<u64>>) -> Result<(), ArchiveError> {
    sink.input = Some(count.clone());
    let mut ar = tar::Archive::new(r);
    let entries = ar.entries().map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
    let mut any = false;
    for entry in entries {
        sink.check_cancel()?;
        let mut entry = entry.map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
        any = true;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let header = entry.header();
        let mode = header.mode().ok();
        let mtime = header.mtime().ok().map(|t| t as i64);
        let kind = header.entry_type();
        use tar::EntryType as T;
        match kind {
            T::Directory => sink.dir(&name, mode, mtime)?,
            T::Regular | T::Continuous | T::GNUSparse => sink.file_from(&name, mode, mtime, &mut entry)?,
            T::Symlink => {
                let target = entry.link_name_bytes().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                sink.symlink(&name, &target)?;
            }
            T::Link => {
                let target = entry.link_name_bytes().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                sink.hardlink(&name, &target)?;
            }
            // Devices, FIFOs and metadata records are never extracted.
            _ => {}
        }
        sink.input_progress(count.get());
    }
    if !any {
        return Err(ArchiveError::Empty);
    }
    Ok(())
}

// --------------------------------------------------------------------- zip

fn zip_error(e: zip::result::ZipError, have_password: bool) -> ArchiveError {
    use zip::result::ZipError as Z;
    match e {
        Z::UnsupportedArchive(m) if m == Z::PASSWORD_REQUIRED => {
            if have_password { ArchiveError::WrongPassword } else { ArchiveError::NeedPassword }
        }
        Z::InvalidPassword => ArchiveError::WrongPassword,
        Z::UnsupportedArchive(m) => ArchiveError::Unsupported(format!("This ZIP uses a feature that isn't supported ({m}).")),
        Z::CompressionMethodNotSupported(m) => ArchiveError::Unsupported(format!("This ZIP uses an unsupported compression method ({m}).")),
        Z::Io(e) => ArchiveError::Corrupt(e.to_string()),
        other => ArchiveError::Corrupt(other.to_string()),
    }
}

fn zip_mtime(dt: Option<zip::DateTime>) -> Option<i64> {
    use chrono::TimeZone;
    let dt = dt?;
    let naive = chrono::NaiveDate::from_ymd_opt(dt.year() as i32, dt.month() as u32, dt.day() as u32)?
        .and_hms_opt(dt.hour() as u32, dt.minute() as u32, dt.second() as u32)?;
    chrono::Local.from_local_datetime(&naive).earliest().map(|t| t.timestamp())
}

fn zip_entries(path: &Path, password: Option<&str>, sink: &mut Sink) -> Result<(), ArchiveError> {
    let (r, count) = Counting::new(BufReader::new(open(path)?));
    sink.input = Some(count.clone());
    let mut ar = zip::ZipArchive::new(r).map_err(|e| zip_error(e, password.is_some()))?;
    if ar.is_empty() {
        return Err(ArchiveError::Empty);
    }
    for i in 0..ar.len() {
        sink.check_cancel()?;
        let mut f = match password {
            Some(p) => ar.by_index_decrypt(i, p.as_bytes()),
            None => ar.by_index(i),
        }
        .map_err(|e| zip_error(e, password.is_some()))?;
        let name = f.name().map(|n| n.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(f.name_raw()).into_owned());
        let mode = f.unix_mode();
        let mtime = zip_mtime(f.last_modified());
        let encrypted = f.encrypted();
        if f.is_dir() {
            sink.dir(&name, mode, mtime)?;
        } else if f.is_symlink() {
            let mut target = String::new();
            f.read_to_string(&mut target).map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            sink.symlink(&name, &target)?;
        } else {
            match sink.file_from(&name, mode, mtime, &mut f) {
                // A wrong ZipCrypto password can pass the header check and
                // only fail the CRC.
                Err(ArchiveError::Corrupt(_)) if encrypted => return Err(ArchiveError::WrongPassword),
                other => other?,
            }
        }
        sink.input_progress(count.get());
    }
    Ok(())
}

// ---------------------------------------------------------------------- 7z

fn sevenz_error(e: sevenz_rust2::Error) -> ArchiveError {
    use sevenz_rust2::Error as E;
    match e {
        E::PasswordRequired => ArchiveError::NeedPassword,
        E::MaybeBadPassword(_) => ArchiveError::WrongPassword,
        E::Io(e, _) => ArchiveError::Corrupt(e.to_string()),
        other => {
            let s = other.to_string();
            if s.to_lowercase().contains("password") {
                ArchiveError::WrongPassword
            } else if s.to_lowercase().contains("unsupported") {
                ArchiveError::Unsupported(format!("This 7-Zip archive uses an unsupported method ({s})."))
            } else {
                ArchiveError::Corrupt(s)
            }
        }
    }
}

fn sevenz_entries(path: &Path, password: Option<&str>, sink: &mut Sink) -> Result<(), ArchiveError> {
    let (r, count) = Counting::new(BufReader::new(open(path)?));
    let pw = match password {
        Some(p) => sevenz_rust2::Password::new(p),
        None => sevenz_rust2::Password::empty(),
    };
    sink.input = Some(count.clone());
    let mut reader = sevenz_rust2::ArchiveReader::new(r, pw).map_err(sevenz_error)?;
    if reader.archive().files.is_empty() {
        return Err(ArchiveError::Empty);
    }
    // The callback can't return our error type; stash it.
    let mut failure: Option<ArchiveError> = None;
    let res = reader.for_each_entries(|entry, data| {
        if entry.is_anti_item() {
            return Ok(true);
        }
        let name = entry.name().to_string();
        let mtime = if entry.has_last_modified_date {
            let nt: u64 = entry.last_modified_date().into();
            // 100 ns ticks since 1601-01-01.
            Some((nt / 10_000_000) as i64 - 11_644_473_600)
        } else {
            None
        };
        // Unix mode is stored in the high 16 bits when bit 15 is set.
        let attrs = entry.windows_attributes();
        let unix = (entry.has_windows_attributes && attrs & 0x8000 != 0).then_some(attrs >> 16);
        let is_link = unix.is_some_and(|m| m & 0o170000 == 0o120000);
        let step = if entry.is_directory() {
            sink.dir(&name, unix, mtime)
        } else if is_link {
            let mut target = String::new();
            match data.read_to_string(&mut target) {
                Ok(_) => sink.symlink(&name, &target),
                Err(e) => Err(ArchiveError::Corrupt(e.to_string())),
            }
        } else {
            sink.file_from(&name, unix, mtime, data)
        };
        sink.input_progress(count.get());
        match step {
            Ok(()) => Ok(true),
            Err(e) => {
                failure = Some(e);
                Ok(false)
            }
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    res.map_err(sevenz_error)
}

// --------------------------------------------------------------------- rar

fn rar_entries(path: &Path, password: Option<&str>, sink: &mut Sink) -> Result<(), ArchiveError> {
    use compress_tools::{ArchiveContents, ArchiveIteratorBuilder};
    let (r, count) = Counting::new(BufReader::new(open(path)?));
    sink.input = Some(count.clone());
    let it = ArchiveIteratorBuilder::new(r).build().map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
    let mut open_file: Option<super::OpenFile> = None;
    let mut any = false;
    for item in it {
        sink.check_cancel()?;
        match item {
            ArchiveContents::StartOfEntry(name, stat) => {
                any = true;
                let mode = stat.st_mode;
                let mtime = Some(stat.st_mtime);
                match mode & libc::S_IFMT {
                    libc::S_IFDIR => sink.dir(&name, Some(mode), mtime)?,
                    libc::S_IFREG => open_file = sink.open_file(&name, Some(mode), mtime)?,
                    // libarchive doesn't expose link targets through this API.
                    _ => {}
                }
            }
            ArchiveContents::DataChunk(chunk) => {
                if let Some(f) = open_file.as_mut() {
                    sink.write(f, &chunk)?;
                }
            }
            ArchiveContents::EndOfEntry => {
                if let Some(f) = open_file.take() {
                    sink.close(f)?;
                }
                sink.input_progress(count.get());
            }
            ArchiveContents::Err(e) => {
                let msg = e.to_string();
                if msg.to_lowercase().contains("encrypt") || msg.to_lowercase().contains("passphrase") {
                    return rar_with_unrar(path, password, sink);
                }
                return Err(ArchiveError::Corrupt(msg));
            }
        }
    }
    if !any {
        return Err(ArchiveError::Empty);
    }
    Ok(())
}

/// Encrypted RAR: libarchive can't decrypt it, so use the system `unrar`
/// (non-free licence, so it can't be bundled), then let the Sink checks run
/// over what it produced.
fn rar_with_unrar(path: &Path, password: Option<&str>, sink: &mut Sink) -> Result<(), ArchiveError> {
    if !crate::terminal::on_path("unrar") {
        return Err(ArchiveError::Unsupported(
            "Password-protected RAR archives need the “unrar” program. Install the unrar package, then try again.".into(),
        ));
    }
    let Some(pw) = password else { return Err(ArchiveError::NeedPassword) };
    // unrar extracts into a private temporary folder; entries are then fed
    // through the Sink so every safety rule still applies.
    let tmp = sink.root.join(".kivo-unrar");
    std::fs::create_dir(&tmp).map_err(|e| ArchiveError::Io(OpError::io(&e, &tmp, "create")))?;
    let status = std::process::Command::new("unrar")
        .args(["x", "-o-", "-y", "-ol-"])
        .arg(format!("-p{pw}"))
        .arg("--")
        .arg(path)
        .arg(format!("{}/", tmp.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| ArchiveError::Io(OpError::io(&e, path, "extract")))?;
    let result = match status.code() {
        Some(0) => import_tree(&tmp, &tmp, sink),
        Some(11) => Err(ArchiveError::WrongPassword),
        Some(c) => Err(ArchiveError::Corrupt(format!("unrar exit code {c}"))),
        None => Err(ArchiveError::Cancelled),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn import_tree(base: &Path, dir: &Path, sink: &mut Sink) -> Result<(), ArchiveError> {
    use std::os::unix::fs::MetadataExt;
    let rd = std::fs::read_dir(dir).map_err(|e| ArchiveError::Io(OpError::io(&e, dir, "read")))?;
    for e in rd.flatten() {
        let p = e.path();
        let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().into_owned();
        let m = std::fs::symlink_metadata(&p).map_err(|e| ArchiveError::Io(OpError::io(&e, &p, "read")))?;
        if m.file_type().is_symlink() {
            let t = std::fs::read_link(&p).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
            sink.symlink(&rel, &t)?;
        } else if m.is_dir() {
            sink.dir(&rel, Some(m.mode()), Some(m.mtime()))?;
            import_tree(base, &p, sink)?;
        } else if m.is_file() {
            let mut f = File::open(&p).map_err(|e| ArchiveError::Io(OpError::io(&e, &p, "read")))?;
            sink.file_from(&rel, Some(m.mode()), Some(m.mtime()), &mut f)?;
        }
    }
    Ok(())
}
