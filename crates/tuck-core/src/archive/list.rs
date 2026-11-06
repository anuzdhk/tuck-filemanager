// SPDX-License-Identifier: GPL-3.0-or-later
//! List an archive's contents without extracting (for Quick Look).

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use super::{ArchiveError, Format, detect};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub format: Option<Format>,
    pub entries: Vec<ListEntry>,
    /// Stopped early (too many entries, or took too long).
    pub truncated: bool,
    /// Some or all content is password-protected.
    pub encrypted: bool,
    /// Total uncompressed size of the listed entries.
    pub total: u64,
}

/// Up to `max` entries, giving up after `budget` (tar streams must be read
/// end to end, so very large ones are cut short).
pub fn list(path: &Path, max: usize, budget: Duration) -> Result<Listing, ArchiveError> {
    let format = detect(path)
        .map_err(|e| ArchiveError::Corrupt(e.to_string()))?
        .ok_or_else(|| ArchiveError::Unsupported("Not an archive.".into()))?;
    let start = Instant::now();
    let mut l = Listing { format: Some(format), ..Default::default() };
    let open = || File::open(path).map_err(|e| ArchiveError::Corrupt(e.to_string()));
    let push = |l: &mut Listing, name: String, size: u64, is_dir: bool| -> bool {
        if l.entries.len() >= max || start.elapsed() > budget {
            l.truncated = true;
            return false;
        }
        l.total += size;
        l.entries.push(ListEntry { name, size, is_dir });
        true
    };
    match format {
        Format::Zip => {
            let mut z = zip::ZipArchive::new(BufReader::new(open()?)).map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            for i in 0..z.len() {
                let f = z.by_index_raw(i).map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
                l.encrypted |= f.encrypted();
                let name = f.name().map(|n| n.into_owned()).unwrap_or_default();
                if !push(&mut l, name, f.size(), f.is_dir()) {
                    break;
                }
            }
        }
        Format::SevenZ => match sevenz_rust2::Archive::read(&mut BufReader::new(open()?), &sevenz_rust2::Password::empty()) {
            Ok(a) => {
                for e in &a.files {
                    if !push(&mut l, e.name().to_string(), e.size(), e.is_directory()) {
                        break;
                    }
                }
            }
            // Encrypted headers: the names themselves are secret.
            Err(sevenz_rust2::Error::PasswordRequired) | Err(sevenz_rust2::Error::MaybeBadPassword(_)) => l.encrypted = true,
            Err(e) => return Err(ArchiveError::Corrupt(e.to_string())),
        },
        Format::Rar => {
            let names = compress_tools::list_archive_files(BufReader::new(open()?)).map_err(|e| {
                if e.to_string().to_lowercase().contains("encrypt") {
                    ArchiveError::NeedPassword
                } else {
                    ArchiveError::Corrupt(e.to_string())
                }
            });
            match names {
                Ok(names) => {
                    for n in names {
                        let dir = n.ends_with('/');
                        if !push(&mut l, n, 0, dir) {
                            break;
                        }
                    }
                }
                Err(ArchiveError::NeedPassword) => l.encrypted = true,
                Err(e) => return Err(e),
            }
        }
        Format::Gz | Format::Bz2 | Format::Xz | Format::Zst => {
            let name = super::stream_output_name(path);
            push(&mut l, name, 0, false);
        }
        Format::Tar | Format::TarGz | Format::TarBz2 | Format::TarXz | Format::TarZst => {
            let reader: Box<dyn Read> = match format {
                Format::Tar => Box::new(BufReader::new(open()?)),
                Format::TarGz => super::read::decoder(super::Stream::Gz, open()?).map_err(|e| ArchiveError::Corrupt(e.to_string()))?,
                Format::TarBz2 => super::read::decoder(super::Stream::Bz2, open()?).map_err(|e| ArchiveError::Corrupt(e.to_string()))?,
                Format::TarXz => super::read::decoder(super::Stream::Xz, open()?).map_err(|e| ArchiveError::Corrupt(e.to_string()))?,
                _ => super::read::decoder(super::Stream::Zst, open()?).map_err(|e| ArchiveError::Corrupt(e.to_string()))?,
            };
            let mut t = tar::Archive::new(reader);
            for e in t.entries().map_err(|e| ArchiveError::Corrupt(e.to_string()))? {
                let e = e.map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
                let name = String::from_utf8_lossy(&e.path_bytes()).into_owned();
                let h = e.header();
                if matches!(h.entry_type(), tar::EntryType::XGlobalHeader | tar::EntryType::XHeader) {
                    continue;
                }
                let is_dir = h.entry_type() == tar::EntryType::Directory;
                if !push(&mut l, name, h.size().unwrap_or(0), is_dir) {
                    break;
                }
            }
        }
    }
    Ok(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{CompressFormat, compress};
    use crate::ops::{Cancel, Fixed, Resolution};

    #[test]
    fn lists_every_format() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("Box");
        std::fs::create_dir_all(src.join("in")).unwrap();
        std::fs::write(src.join("in/a.txt"), "12345").unwrap();
        for f in CompressFormat::ALL {
            let a = compress(std::slice::from_ref(&src), d.path(), &format!("b{}", f.extension()), f, None, &mut Fixed(Resolution::Skip), &Cancel::new()).unwrap();
            let l = list(&a, 100, Duration::from_secs(5)).unwrap();
            assert!(l.entries.iter().any(|e| e.name.ends_with("in/a.txt") && e.size == 5), "{f:?}: {:?}", l.entries);
            assert!(!l.encrypted);
        }
        let z = compress(std::slice::from_ref(&src), d.path(), "p.zip", CompressFormat::Zip, Some("pw"), &mut Fixed(Resolution::Skip), &Cancel::new()).unwrap();
        let l = list(&z, 100, Duration::from_secs(5)).unwrap();
        assert!(l.encrypted && !l.entries.is_empty(), "zip names stay visible");
        let s = compress(&[src], d.path(), "p.7z", CompressFormat::SevenZ, Some("pw"), &mut Fixed(Resolution::Skip), &Cancel::new()).unwrap();
        let l = list(&s, 100, Duration::from_secs(5)).unwrap();
        assert!(l.encrypted && l.entries.is_empty(), "7z header encryption hides names");
        let l = list(&z, 1, Duration::from_secs(5)).unwrap();
        assert!(l.truncated && l.entries.len() == 1);
    }
}
