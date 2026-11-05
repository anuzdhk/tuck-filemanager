// SPDX-License-Identifier: GPL-3.0-or-later
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use super::*;
use crate::ops::{Cancel, Fixed, Resolution};

fn obs() -> Fixed {
    Fixed(Resolution::Skip)
}

fn run(archive: &Path) -> Result<Extracted, ArchiveError> {
    extract(archive, None, Limits::default(), &mut obs(), &Cancel::new())
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
    let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let o = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        if name.ends_with('/') {
            z.add_directory(*name, o).unwrap();
        } else {
            z.start_file(*name, o).unwrap();
            z.write_all(data).unwrap();
        }
    }
    z.finish().unwrap();
}

/// A tar entry with any name at all (the tar crate refuses "..").
fn raw_tar_entry(out: &mut Vec<u8>, name: &str, kind: u8, link: &str, data: &[u8], mode: u32) {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(format!("{mode:07o}").as_bytes());
    h[108..115].copy_from_slice(b"0001750");
    h[116..123].copy_from_slice(b"0001750");
    h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
    h[136..147].copy_from_slice(b"14000000000");
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
}

fn finish_tar(mut v: Vec<u8>) -> Vec<u8> {
    v.extend_from_slice(&[0u8; 1024]);
    v
}

fn no_leftovers(dir: &Path) {
    for n in listing(dir) {
        assert!(!n.starts_with(".kivo-partial"), "leftover staging: {n}");
    }
}

#[test]
fn detects_by_content_not_name() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    let src = p.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("a.txt"), "a").unwrap();
    for f in CompressFormat::ALL {
        let out = compress(std::slice::from_ref(&src), p, &format!("x{}", f.extension()), f, None, &mut obs(), &Cancel::new()).unwrap();
        let expected = match f {
            CompressFormat::Zip => Format::Zip,
            CompressFormat::TarGz => Format::TarGz,
            CompressFormat::TarXz => Format::TarXz,
            CompressFormat::TarZst => Format::TarZst,
            CompressFormat::SevenZ => Format::SevenZ,
        };
        assert_eq!(detect(&out).unwrap(), Some(expected), "{f:?}");
        // Renamed to .txt: still detected.
        let renamed = p.join(format!("disguised-{f:?}.txt"));
        fs::copy(&out, &renamed).unwrap();
        assert_eq!(detect(&renamed).unwrap(), Some(expected));
    }
    let mut gz = flate2::write::GzEncoder::new(fs::File::create(p.join("n.txt.gz")).unwrap(), flate2::Compression::fast());
    gz.write_all(b"plain text, not a tar").unwrap();
    gz.finish().unwrap();
    assert_eq!(detect(&p.join("n.txt.gz")).unwrap(), Some(Format::Gz));
    let mut bz = bzip2::write::BzEncoder::new(fs::File::create(p.join("t.tar.bz2")).unwrap(), bzip2::Compression::fast());
    let mut b = tar::Builder::new(Vec::new());
    b.append_path_with_name(src.join("a.txt"), "a.txt").unwrap();
    bz.write_all(&b.into_inner().unwrap()).unwrap();
    bz.finish().unwrap();
    assert_eq!(detect(&p.join("t.tar.bz2")).unwrap(), Some(Format::TarBz2));
    fs::write(p.join("random.bin"), [0x13u8; 600]).unwrap();
    assert_eq!(detect(&p.join("random.bin")).unwrap(), None);
}

#[test]
fn many_items_go_into_a_named_folder_never_overwriting() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("Holiday Photos.zip");
    zip_with(&a, &[("one.jpg", b"1"), ("two.jpg", b"2")]);
    let r = run(&a).unwrap();
    assert_eq!(r.path, d.path().join("Holiday Photos"));
    assert_eq!(listing(&r.path), ["one.jpg", "two.jpg"]);
    let mode = fs::metadata(&r.path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    let r2 = run(&a).unwrap();
    assert_eq!(r2.path, d.path().join("Holiday Photos (2)"));
    no_leftovers(d.path());
}

#[test]
fn single_top_level_item_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("download.zip");
    zip_with(&a, &[("Project/", b""), ("Project/readme.md", b"hi"), ("Project/src/", b""), ("Project/src/main.rs", b"fn main(){}")]);
    let r = run(&a).unwrap();
    assert_eq!(r.path, d.path().join("Project"));
    assert_eq!(fs::read(r.path.join("src/main.rs")).unwrap(), b"fn main(){}");
    // A single file too.
    let b = d.path().join("one.zip");
    zip_with(&b, &[("report.pdf", b"%PDF")]);
    assert_eq!(run(&b).unwrap().path, d.path().join("report.pdf"));
    assert_eq!(run(&b).unwrap().path, d.path().join("report (2).pdf"));
}

#[test]
fn single_stream_gets_its_inner_name() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("notes.txt.gz");
    let mut gz = flate2::write::GzEncoder::new(fs::File::create(&a).unwrap(), flate2::Compression::default());
    gz.write_all(b"hello notes").unwrap();
    gz.finish().unwrap();
    let r = run(&a).unwrap();
    assert_eq!(r.path, d.path().join("notes.txt"));
    assert_eq!(fs::read(&r.path).unwrap(), b"hello notes");
    {
        let (name, data) = ("log.zst", zstd::encode_all(&b"zstd data"[..], 3).unwrap());
        let p = d.path().join(name);
        fs::write(&p, data).unwrap();
        assert_eq!(fs::read(run(&p).unwrap().path).unwrap(), b"zstd data");
    }
}

#[test]
fn traversal_aborts_everything() {
    let d = tempfile::tempdir().unwrap();
    let box_ = d.path().join("box");
    fs::create_dir(&box_).unwrap();
    let a = box_.join("evil.zip");
    zip_with(&a, &[("fine.txt", b"ok"), ("../escaped.txt", b"pwned")]);
    let e = run(&a).unwrap_err();
    assert!(matches!(e, ArchiveError::Unsafe(_)), "{e:?}");
    assert!(!d.path().join("escaped.txt").exists());
    assert_eq!(listing(&box_), ["evil.zip"], "nothing extracted, nothing left");

    // Same in tar, with a deeper climb hidden mid-path.
    let mut t = Vec::new();
    raw_tar_entry(&mut t, "a/b/../../../escaped2.txt", b'0', "", b"pwned", 0o644);
    fs::write(box_.join("evil.tar"), finish_tar(t)).unwrap();
    assert!(matches!(run(&box_.join("evil.tar")).unwrap_err(), ArchiveError::Unsafe(_)));
    assert!(!d.path().join("escaped2.txt").exists());
    no_leftovers(&box_);
}

#[test]
fn absolute_paths_are_made_relative() {
    let d = tempfile::tempdir().unwrap();
    let mut t = Vec::new();
    raw_tar_entry(&mut t, "/etc/kivo-test-should-not-exist", b'0', "", b"x", 0o644);
    raw_tar_entry(&mut t, "other.txt", b'0', "", b"y", 0o644);
    let a = d.path().join("abs.tar");
    fs::write(&a, finish_tar(t)).unwrap();
    let r = run(&a).unwrap();
    assert!(!Path::new("/etc/kivo-test-should-not-exist").exists());
    assert!(r.path.join("etc/kivo-test-should-not-exist").exists());
    assert_eq!(sanitize("C:\\Windows\\x.txt").unwrap(), PathBuf::from("Windows/x.txt"));
    assert_eq!(sanitize("./a/./b/../c").unwrap(), PathBuf::from("a/c"));
}

#[test]
fn unsafe_symlinks_are_left_out() {
    let d = tempfile::tempdir().unwrap();
    let mut t = Vec::new();
    raw_tar_entry(&mut t, "pkg/", b'5', "", b"", 0o755);
    raw_tar_entry(&mut t, "pkg/real.txt", b'0', "", b"data", 0o644);
    raw_tar_entry(&mut t, "pkg/good-link", b'2', "real.txt", b"", 0o777);
    raw_tar_entry(&mut t, "pkg/abs-link", b'2', "/etc/passwd", b"", 0o777);
    raw_tar_entry(&mut t, "pkg/up-link", b'2', "../../outside", b"", 0o777);
    // Chain trick: each target looks harmless, together they climb out.
    raw_tar_entry(&mut t, "pkg/self", b'2', ".", b"", 0o777);
    raw_tar_entry(&mut t, "pkg/sub/", b'5', "", b"", 0o755);
    raw_tar_entry(&mut t, "pkg/sneaky", b'2', "self/sub/../..", b"", 0o777);
    let a = d.path().join("links.tar");
    fs::write(&a, finish_tar(t)).unwrap();
    let r = run(&a).unwrap();
    let root = &r.path;
    assert_eq!(fs::read_link(root.join("good-link")).unwrap(), Path::new("real.txt"));
    assert!(fs::symlink_metadata(root.join("abs-link")).is_err());
    assert!(fs::symlink_metadata(root.join("up-link")).is_err());
    assert!(fs::symlink_metadata(root.join("sneaky")).is_err(), "chain escape removed");
    assert_eq!(r.skipped_links, 3);
}

#[test]
fn nothing_is_written_through_a_symlink() {
    let d = tempfile::tempdir().unwrap();
    let outside = d.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let box_ = d.path().join("box");
    fs::create_dir(&box_).unwrap();
    let mut t = Vec::new();
    raw_tar_entry(&mut t, "x/", b'5', "", b"", 0o755);
    raw_tar_entry(&mut t, "x/door", b'2', &format!("{}", outside.display()), b"", 0o777);
    raw_tar_entry(&mut t, "x/door/payload.txt", b'0', "", b"pwned", 0o644);
    let a = box_.join("door.tar");
    fs::write(&a, finish_tar(t)).unwrap();
    let r = run(&a).unwrap();
    assert!(listing(&outside).is_empty(), "outside untouched");
    assert!(r.path.join("door/payload.txt").is_file(), "written as a plain folder instead");
}

#[test]
fn bombs_and_entry_floods_stop_cleanly() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("zeros.gz");
    let mut gz = flate2::write::GzEncoder::new(fs::File::create(&a).unwrap(), flate2::Compression::best());
    gz.write_all(&vec![0u8; 8 * 1024 * 1024]).unwrap();
    gz.finish().unwrap();
    let limits = Limits { ratio_floor: 1024 * 1024, max_ratio: 20, ..Limits::default() };
    let e = extract(&a, None, limits, &mut obs(), &Cancel::new()).unwrap_err();
    assert!(matches!(e, ArchiveError::TooBig(_)), "{e:?}");
    assert_eq!(listing(d.path()), ["zeros.gz"]);

    let many = d.path().join("many.zip");
    let entries: Vec<(String, Vec<u8>)> = (0..50).map(|i| (format!("f{i}"), vec![])).collect();
    let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    zip_with(&many, &refs);
    let limits = Limits { max_entries: 10, ..Limits::default() };
    assert!(matches!(extract(&many, None, limits, &mut obs(), &Cancel::new()), Err(ArchiveError::TooBig(_))));
    no_leftovers(d.path());
}

#[test]
fn zip_passwords() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("Secret");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("plan.txt"), "the plan").unwrap();
    let a = compress(std::slice::from_ref(&src), d.path(), "s.zip", CompressFormat::Zip, Some("hunter2"), &mut obs(), &Cancel::new()).unwrap();
    fs::remove_dir_all(&src).unwrap();
    assert_eq!(run(&a).unwrap_err(), ArchiveError::NeedPassword);
    assert_eq!(extract(&a, Some("nope"), Limits::default(), &mut obs(), &Cancel::new()).unwrap_err(), ArchiveError::WrongPassword);
    let r = extract(&a, Some("hunter2"), Limits::default(), &mut obs(), &Cancel::new()).unwrap();
    assert_eq!(fs::read(r.path.join("plan.txt")).unwrap(), b"the plan");
    no_leftovers(d.path());
}

#[test]
fn sevenz_passwords() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("Vault");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("k.txt"), "key").unwrap();
    let a = compress(std::slice::from_ref(&src), d.path(), "v.7z", CompressFormat::SevenZ, Some("pw"), &mut obs(), &Cancel::new()).unwrap();
    fs::remove_dir_all(&src).unwrap();
    assert_eq!(run(&a).unwrap_err(), ArchiveError::NeedPassword);
    let wrong = extract(&a, Some("bad"), Limits::default(), &mut obs(), &Cancel::new()).unwrap_err();
    assert_eq!(wrong, ArchiveError::WrongPassword);
    let r = extract(&a, Some("pw"), Limits::default(), &mut obs(), &Cancel::new()).unwrap();
    assert_eq!(fs::read(r.path.join("k.txt")).unwrap(), b"key");
    no_leftovers(d.path());
}

#[test]
fn every_format_round_trips() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("Stuff");
    fs::create_dir_all(src.join("deep/er")).unwrap();
    fs::write(src.join("a.txt"), "alpha").unwrap();
    fs::write(src.join("deep/er/b.bin"), vec![9u8; 300_000]).unwrap();
    fs::set_permissions(src.join("a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    symlink("a.txt", src.join("link")).unwrap();
    for f in CompressFormat::ALL {
        let out_dir = d.path().join(format!("out-{f:?}"));
        fs::create_dir(&out_dir).unwrap();
        let a = compress(std::slice::from_ref(&src), &out_dir, &format!("Stuff{}", f.extension()), f, None, &mut obs(), &Cancel::new()).unwrap();
        let r = run(&a).unwrap();
        assert_eq!(r.path, out_dir.join("Stuff"), "{f:?}");
        assert_eq!(fs::read(r.path.join("a.txt")).unwrap(), b"alpha", "{f:?}");
        assert_eq!(fs::read(r.path.join("deep/er/b.bin")).unwrap().len(), 300_000, "{f:?}");
        if f != CompressFormat::SevenZ {
            assert_eq!(fs::read_link(r.path.join("link")).unwrap(), Path::new("a.txt"), "{f:?}");
            assert_eq!(fs::metadata(r.path.join("a.txt")).unwrap().permissions().mode() & 0o777, 0o640, "{f:?}");
        }
        no_leftovers(&out_dir);
    }
}

#[test]
fn compress_never_overwrites() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a.txt"), "a").unwrap();
    fs::write(d.path().join("a.zip"), "existing").unwrap();
    let out = compress(&[d.path().join("a.txt")], d.path(), "a.zip", CompressFormat::Zip, None, &mut obs(), &Cancel::new()).unwrap();
    assert_eq!(out, d.path().join("a (2).zip"));
    assert_eq!(fs::read(d.path().join("a.zip")).unwrap(), b"existing");
}

#[test]
fn cancel_leaves_nothing() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("big");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("f.bin"), vec![1u8; 4 * 1024 * 1024]).unwrap();
    let cancel = Cancel::new();
    cancel.cancel();
    assert_eq!(
        compress(std::slice::from_ref(&src), d.path(), "big.zip", CompressFormat::Zip, None, &mut obs(), &cancel).unwrap_err(),
        ArchiveError::Cancelled
    );
    let a = compress(&[src], d.path(), "big.tar.gz", CompressFormat::TarGz, None, &mut obs(), &Cancel::new()).unwrap();
    assert_eq!(extract(&a, None, Limits::default(), &mut obs(), &cancel).unwrap_err(), ArchiveError::Cancelled);
    assert_eq!(listing(d.path()), ["big", "big.tar.gz"]);
}

#[test]
fn setuid_is_dropped_and_owner_can_write() {
    let d = tempfile::tempdir().unwrap();
    let mut t = Vec::new();
    raw_tar_entry(&mut t, "tool", b'0', "", b"#!/bin/sh", 0o4555);
    let a = d.path().join("t.tar");
    fs::write(&a, finish_tar(t)).unwrap();
    let r = run(&a).unwrap();
    assert_eq!(fs::metadata(&r.path).unwrap().permissions().mode() & 0o7777, 0o755);
}

#[test]
fn damaged_and_empty_archives_are_explained() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("cut.zip");
    zip_with(&a, &[("a.txt", &[7u8; 5000])]);
    let bytes = fs::read(&a).unwrap();
    fs::write(&a, &bytes[..bytes.len() / 2]).unwrap();
    assert!(matches!(run(&a).unwrap_err(), ArchiveError::Corrupt(_)));
    let e = d.path().join("empty.tar");
    fs::write(&e, [0u8; 1024]).unwrap();
    // An all-zero tar has no valid header, so it isn't recognised at all.
    assert!(run(&e).is_err());
    no_leftovers(d.path());
    let msg = ArchiveError::Unsafe("x".into());
    assert!(msg.message("evil.zip").contains("outside"));
    assert!(msg.hint().contains("nothing was extracted"));
}

#[test]
fn documents_that_are_zips_open_instead_of_extracting() {
    assert!(!is_extractable(Path::new("Report.docx"), Some(Format::Zip)));
    assert!(!is_extractable(Path::new("book.epub"), Some(Format::Zip)));
    assert!(is_extractable(Path::new("photos.zip"), Some(Format::Zip)));
    assert!(is_extractable(Path::new("disguised.txt"), Some(Format::Zip)));
    assert!(is_extractable(Path::new("x.tar.gz"), Some(Format::TarGz)));
    assert!(!is_extractable(Path::new("x.zip"), None));
    assert_eq!(folder_name_for(Path::new("/a/Backup.tar.gz")), "Backup");
    assert_eq!(folder_name_for(Path::new("/a/README")), "README contents");
}

#[test]
fn rar_extracts_through_libarchive() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("tree.rar");
    fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tree.rar"), &a).unwrap();
    assert_eq!(detect(&a).unwrap(), Some(Format::Rar));
    let r = run(&a).unwrap();
    fn walk(p: &Path, out: &mut Vec<String>, base: &Path) {
        for e in fs::read_dir(p).unwrap().flatten() {
            out.push(e.path().strip_prefix(base).unwrap().display().to_string());
            if e.path().is_dir() {
                walk(&e.path(), out, base);
            }
        }
    }
    let mut all = Vec::new();
    if r.path.is_dir() {
        walk(&r.path, &mut all, d.path());
    }
    all.sort();
    assert_eq!(r.path, d.path().join("tree"));
    assert_eq!(all, ["tree/branch1", "tree/branch1/leaf", "tree/branch2", "tree/branch2/leaf"]);
    no_leftovers(d.path());
}
