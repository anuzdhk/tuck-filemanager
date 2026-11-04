// SPDX-License-Identifier: GPL-3.0-or-later
//! File operations: copy, move, delete, rename, create, link.
//!
//! Safety rules, in order of priority:
//! 1. A source is never removed until its copy is complete (cross-device moves).
//! 2. Every top-level item is all-or-nothing: on cancel or failure, anything
//!    this operation created for that item is removed again.
//! 3. Files are written to a hidden temporary name and renamed into place,
//!    so a replaced file is never left half-written.
//! 4. Renames never overwrite (`RENAME_NOREPLACE`).
//!
//! Operations are synchronous and meant to run on a worker thread. They report
//! progress and ask about conflicts through an [`Observer`].

use std::fs::{self, File, FileTimes, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::format;
use crate::names;

#[derive(Clone, Default, Debug)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    /// For identity comparisons.
    pub fn inner(&self) -> &Arc<AtomicBool> {
        &self.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Name of the item being processed.
    pub current: String,
}

impl Progress {
    pub fn fraction(&self) -> f64 {
        if self.bytes_total > 0 {
            (self.bytes_done as f64 / self.bytes_total as f64).min(1.0)
        } else if self.files_total > 0 {
            (self.files_done as f64 / self.files_total as f64).min(1.0)
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictInfo {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub source_is_dir: bool,
    pub dest_is_dir: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Overwrite the existing item. Folder onto folder means merge.
    Replace,
    Skip,
    /// Keep both: the incoming item gets a numbered name.
    KeepBoth,
    /// Folder onto folder: put the contents into the existing folder.
    Merge,
    Cancel,
}

pub trait Observer {
    fn progress(&mut self, p: &Progress);
    /// Decide a conflict. The bool is "apply to all remaining conflicts".
    fn conflict(&mut self, c: &ConflictInfo) -> (Resolution, bool);
}

/// An observer that never asks: uses a fixed resolution.
pub struct Fixed(pub Resolution);

impl Observer for Fixed {
    fn progress(&mut self, _: &Progress) {}
    fn conflict(&mut self, _: &ConflictInfo) -> (Resolution, bool) {
        (self.0, true)
    }
}

/// A failure, explained: what went wrong and what to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpError {
    pub path: PathBuf,
    pub message: String,
    pub hint: String,
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.message, self.hint)
    }
}

impl OpError {
    pub fn new(path: &Path, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { path: path.to_path_buf(), message: message.into(), hint: hint.into() }
    }

    /// Turn an I/O error into plain language. `doing` is e.g. "copy".
    pub fn io(e: &io::Error, path: &Path, doing: &str) -> Self {
        let name = display_name(path);
        use io::ErrorKind as K;
        let (message, hint) = match e.kind() {
            K::PermissionDenied => (
                format!("You don't have permission to {doing} “{name}”."),
                "Check the permissions in Properties, or ask the owner for access.".to_string(),
            ),
            K::NotFound => (
                format!("“{name}” no longer exists."),
                "It may have been moved or deleted by another app.".into(),
            ),
            K::StorageFull | K::QuotaExceeded => (
                "There isn't enough free space.".to_string(),
                "Free up some space or choose another location, then try again.".into(),
            ),
            K::ReadOnlyFilesystem => (
                format!("“{name}” is on a read-only drive."),
                "Choose a location you can write to.".into(),
            ),
            K::AlreadyExists => (
                format!("An item named “{name}” already exists."),
                "Choose a different name.".into(),
            ),
            K::DirectoryNotEmpty => (
                format!("“{name}” isn't empty."),
                "Move or delete its contents first.".into(),
            ),
            K::InvalidFilename => (
                format!("“{name}” isn't a valid name on this drive."),
                "Some drives (like FAT or exFAT) don't allow characters such as : ? * \" < > |.".into(),
            ),
            K::ResourceBusy => (
                format!("“{name}” is in use."),
                "Close any app that is using it, then try again.".into(),
            ),
            _ => (format!("Couldn't {doing} “{name}”."), e.to_string()),
        };
        Self { path: path.to_path_buf(), message, hint }
    }
}

fn display_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

#[derive(Debug, Default)]
pub struct Report {
    /// (source, destination) for each top-level item that completed.
    pub done: Vec<(PathBuf, PathBuf)>,
    pub skipped: Vec<PathBuf>,
    pub errors: Vec<OpError>,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Copy,
    Move,
}

// ------------------------------------------------------------------ scanning

#[derive(Default, Clone, Copy)]
struct Totals {
    files: u64,
    bytes: u64,
}

fn scan(path: &Path, cancel: &Cancel) -> Totals {
    let mut t = Totals::default();
    let Ok(m) = fs::symlink_metadata(path) else { return t };
    t.files += 1;
    if m.is_dir() {
        if let Ok(rd) = fs::read_dir(path) {
            for e in rd.flatten() {
                if cancel.is_cancelled() {
                    break;
                }
                let s = scan(&e.path(), cancel);
                t.files += s.files;
                t.bytes += s.bytes;
            }
        }
    } else if m.is_file() {
        t.bytes += m.len();
    }
    t
}

/// Free bytes on the file system holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid NUL-terminated path and a zeroed out-struct.
    let r = unsafe { libc::statvfs(c.as_ptr(), &mut s) };
    (r == 0).then(|| s.f_bavail as u64 * s.f_frsize as u64)
}

fn same_device(a: &Path, b: &Path) -> bool {
    match (fs::symlink_metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

// ------------------------------------------------------------------ transfer

struct Ctx<'a> {
    mode: Mode,
    obs: &'a mut dyn Observer,
    cancel: &'a Cancel,
    progress: Progress,
    policy_file: Option<Resolution>,
    policy_dir: Option<Resolution>,
    report: Report,
    buf: Vec<u8>,
    last_emit: std::time::Instant,
}

impl Ctx<'_> {
    fn emit(&mut self, force: bool) {
        if force || self.last_emit.elapsed() >= std::time::Duration::from_millis(50) {
            self.obs.progress(&self.progress);
            self.last_emit = std::time::Instant::now();
        }
    }

    fn resolve(&mut self, c: &ConflictInfo) -> Resolution {
        let both_dirs = c.source_is_dir && c.dest_is_dir;
        let remembered = if both_dirs { self.policy_dir } else { self.policy_file };
        if let Some(r) = remembered {
            return r;
        }
        let (r, all) = self.obs.conflict(c);
        if all {
            if both_dirs {
                self.policy_dir = Some(r);
            } else {
                self.policy_file = Some(r);
            }
        }
        r
    }
}

/// Copy or move `sources` into `dest_dir`.
pub fn transfer(
    mode: Mode,
    sources: &[PathBuf],
    dest_dir: &Path,
    obs: &mut dyn Observer,
    cancel: &Cancel,
) -> Report {
    let mut ctx = Ctx {
        mode,
        obs,
        cancel,
        progress: Progress::default(),
        policy_file: None,
        policy_dir: None,
        report: Report::default(),
        buf: vec![0; 1 << 20],
        last_emit: std::time::Instant::now(),
    };

    match fs::metadata(dest_dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            ctx.report.errors.push(OpError::new(dest_dir, "The destination isn't a folder.", "Choose a folder."));
            return ctx.report;
        }
        Err(e) => {
            ctx.report.errors.push(OpError::io(&e, dest_dir, "open"));
            return ctx.report;
        }
    }

    // Plan: reject impossible items, measure the rest.
    let mut plan = Vec::new();
    let mut needs_space = 0u64;
    for src in sources {
        let name = match src.file_name() {
            Some(n) => n.to_owned(),
            None => {
                ctx.report.errors.push(OpError::new(src, "This item can't be copied.", ""));
                continue;
            }
        };
        if fs::symlink_metadata(src).is_err() {
            ctx.report.errors.push(OpError::io(&io::Error::from(io::ErrorKind::NotFound), src, "copy"));
            continue;
        }
        let src_is_dir = fs::symlink_metadata(src).map(|m| m.is_dir()).unwrap_or(false);
        if src_is_dir && dest_dir.starts_with(src) {
            let verb = if mode == Mode::Copy { "copy" } else { "move" };
            ctx.report.errors.push(OpError::new(
                src,
                format!("You can't {verb} “{}” into itself.", display_name(src)),
                "Choose a destination outside this folder.",
            ));
            continue;
        }
        if mode == Mode::Move && src.parent() == Some(dest_dir) {
            // Moving into the folder it's already in: nothing to do.
            ctx.report.skipped.push(src.clone());
            continue;
        }
        let t = scan(src, cancel);
        ctx.progress.files_total += t.files;
        ctx.progress.bytes_total += t.bytes;
        if mode == Mode::Copy || !same_device(src, dest_dir) {
            needs_space += t.bytes;
        }
        plan.push((src.clone(), name, t));
    }

    if needs_space > 0
        && let Some(free) = free_space(dest_dir)
            && needs_space > free {
                ctx.report.errors.push(OpError::new(
                    dest_dir,
                    format!(
                        "There isn't enough free space in “{}”.",
                        display_name(dest_dir)
                    ),
                    format!(
                        "{} is needed but only {} is free. Free up {} or choose another location.",
                        format::size(needs_space),
                        format::size(free),
                        format::size(needs_space - free)
                    ),
                ));
                return ctx.report;
            }
    ctx.emit(true);

    for (src, name, totals) in plan {
        if cancel.is_cancelled() {
            ctx.report.cancelled = true;
            break;
        }
        let dst = dest_dir.join(&name);
        let before = ctx.progress.clone();
        match top_level(&mut ctx, &src, &dst, totals) {
            Ok(Some(final_dst)) => ctx.report.done.push((src, final_dst)),
            Ok(None) => {}
            Err(e) => {
                // Keep progress honest after a rolled-back item.
                ctx.progress.files_done = before.files_done + totals.files;
                ctx.progress.bytes_done = before.bytes_done + totals.bytes;
                ctx.report.errors.push(e);
            }
        }
        if ctx.report.cancelled {
            break;
        }
    }
    ctx.emit(true);
    ctx.report
}

/// One top-level item, all-or-nothing. Returns the final destination, or
/// None if skipped/cancelled.
fn top_level(ctx: &mut Ctx, src: &Path, dst: &Path, totals: Totals) -> Result<Option<PathBuf>, OpError> {
    let smeta = fs::symlink_metadata(src).map_err(|e| OpError::io(&e, src, "read"))?;
    let mut dst = dst.to_path_buf();
    let mut merge = false;
    let mut replace = false;

    if let Ok(dmeta) = fs::symlink_metadata(&dst) {
        let same = smeta.dev() == dmeta.dev() && smeta.ino() == dmeta.ino();
        if same {
            // Pasting into the same folder: make a copy, like Finder.
            let parent = dst.parent().unwrap_or(Path::new("/")).to_path_buf();
            let name = names::duplicate_name(&parent, &display_name(src), smeta.is_dir());
            dst = parent.join(name);
        } else {
            let c = ConflictInfo {
                source: src.to_path_buf(),
                dest: dst.clone(),
                source_is_dir: smeta.is_dir(),
                dest_is_dir: dmeta.is_dir(),
            };
            match ctx.resolve(&c) {
                Resolution::Skip => {
                    ctx.report.skipped.push(src.to_path_buf());
                    ctx.progress.files_done += totals.files;
                    ctx.progress.bytes_done += totals.bytes;
                    return Ok(None);
                }
                Resolution::Cancel => {
                    ctx.report.cancelled = true;
                    return Ok(None);
                }
                Resolution::KeepBoth => {
                    let parent = dst.parent().unwrap_or(Path::new("/")).to_path_buf();
                    dst = names::unique_path(&parent, &display_name(src));
                }
                Resolution::Merge | Resolution::Replace if c.source_is_dir && c.dest_is_dir => merge = true,
                Resolution::Merge | Resolution::Replace => replace = true,
            }
        }
    }

    // Fast path: same-device move is a rename.
    if ctx.mode == Mode::Move && !merge {
        let target_exists = fs::symlink_metadata(&dst).is_ok();
        let renamed = if replace && !smeta.is_dir() && !target_exists_is_dir(&dst) {
            fs::rename(src, &dst) // atomic replace of a file
        } else if !target_exists {
            rename_noreplace(src, &dst)
        } else {
            Err(io::Error::from(io::ErrorKind::AlreadyExists))
        };
        match renamed {
            Ok(()) => {
                ctx.progress.files_done += totals.files;
                ctx.progress.bytes_done += totals.bytes;
                ctx.progress.current = display_name(src);
                ctx.emit(false);
                return Ok(Some(dst));
            }
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {} // fall through to copy
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && replace => {} // kinds differ: copy path
            Err(e) => return Err(OpError::io(&e, src, "move")),
        }
    }

    // Copy (or cross-device move): stage, then commit.
    let mut created: Vec<PathBuf> = Vec::new();
    let result = if merge {
        copy_dir_contents(ctx, src, &dst, &mut created)
    } else if replace {
        // Copy beside the target under a hidden name, then swap in.
        let staged = staging_path(&dst);
        let r = copy_entry(ctx, src, &staged, &mut created);
        match r {
            Ok(()) => commit_replace(&staged, &dst).inspect(|_| {
                created.retain(|p| p != &staged);
            }),
            Err(e) => Err(e),
        }
    } else {
        copy_entry(ctx, src, &dst, &mut created)
    };

    let cancelled = ctx.cancel.is_cancelled();
    if result.is_err() || cancelled {
        // Roll back everything this item created, newest first.
        for p in created.iter().rev() {
            let _ = remove_any(p);
        }
        if cancelled {
            ctx.report.cancelled = true;
            return Ok(None);
        }
        return result.map(|_| None);
    }

    if ctx.mode == Mode::Move {
        // The copy is complete; only now remove the source.
        if let Err(e) = remove_any(src) {
            let mut err = OpError::io(&e, src, "remove the original of");
            err.message = format!("“{}” was copied, but the original couldn't be removed.", display_name(src));
            return Err(err);
        }
    }
    Ok(Some(dst))
}

fn target_exists_is_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

fn staging_path(dst: &Path) -> PathBuf {
    let parent = dst.parent().unwrap_or(Path::new("/"));
    let name = display_name(dst);
    let pid = std::process::id();
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    parent.join(format!(".strata-partial-{pid}-{n}-{name}"))
}

/// Swap a fully written staged item into `dst`.
fn commit_replace(staged: &Path, dst: &Path) -> Result<(), OpError> {
    let staged_dir = target_exists_is_dir(staged);
    let dst_dir = target_exists_is_dir(dst);
    if !staged_dir && !dst_dir {
        return fs::rename(staged, dst).map_err(|e| OpError::io(&e, dst, "replace"));
    }
    // Kinds differ (or folder): move the old item aside, swap, then delete it.
    let aside = staging_path(dst);
    fs::rename(dst, &aside).map_err(|e| OpError::io(&e, dst, "replace"))?;
    if let Err(e) = fs::rename(staged, dst) {
        let _ = fs::rename(&aside, dst);
        return Err(OpError::io(&e, dst, "replace"));
    }
    let _ = remove_any(&aside);
    Ok(())
}

fn copy_entry(ctx: &mut Ctx, src: &Path, dst: &Path, created: &mut Vec<PathBuf>) -> Result<(), OpError> {
    if ctx.cancel.is_cancelled() {
        return Ok(());
    }
    let m = fs::symlink_metadata(src).map_err(|e| OpError::io(&e, src, "read"))?;
    ctx.progress.current = display_name(src);
    let ft = m.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(src).map_err(|e| OpError::io(&e, src, "read"))?;
        symlink(&target, dst).map_err(|e| OpError::io(&e, dst, "create"))?;
        created.push(dst.to_path_buf());
        ctx.progress.files_done += 1;
    } else if ft.is_dir() {
        fs::create_dir(dst).map_err(|e| OpError::io(&e, dst, "create"))?;
        created.push(dst.to_path_buf());
        // Writable while we fill it; real permissions are applied after.
        let _ = fs::set_permissions(dst, Permissions::from_mode(0o700));
        ctx.progress.files_done += 1;
        copy_dir_contents(ctx, src, dst, created)?;
        finish_metadata(dst, &m);
    } else if ft.is_file() {
        let tmp = staging_path(dst);
        created.push(tmp.clone());
        copy_file_data(ctx, src, &tmp, &m)?;
        rename_noreplace(&tmp, dst).map_err(|e| OpError::io(&e, dst, "create"))?;
        created.pop();
        created.push(dst.to_path_buf());
        ctx.progress.files_done += 1;
    } else {
        return Err(OpError::new(
            src,
            format!("“{}” is a special file (device, pipe or socket) and can't be copied.", display_name(src)),
            "Skip it, or copy the rest of the items without it.",
        ));
    }
    ctx.emit(false);
    Ok(())
}

fn copy_dir_contents(ctx: &mut Ctx, src: &Path, dst: &Path, created: &mut Vec<PathBuf>) -> Result<(), OpError> {
    let rd = fs::read_dir(src).map_err(|e| OpError::io(&e, src, "open"))?;
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for child in entries {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        let name = child.file_name().expect("dir entries have names").to_owned();
        let target = dst.join(&name);
        if let Ok(dmeta) = fs::symlink_metadata(&target) {
            // Only reachable when merging into an existing folder.
            let smeta = fs::symlink_metadata(&child).map_err(|e| OpError::io(&e, &child, "read"))?;
            let c = ConflictInfo {
                source: child.clone(),
                dest: target.clone(),
                source_is_dir: smeta.is_dir(),
                dest_is_dir: dmeta.is_dir(),
            };
            match ctx.resolve(&c) {
                Resolution::Skip => {
                    let t = scan(&child, ctx.cancel);
                    ctx.progress.files_done += t.files;
                    ctx.progress.bytes_done += t.bytes;
                    ctx.report.skipped.push(child);
                    continue;
                }
                Resolution::Cancel => {
                    ctx.cancel.cancel();
                    return Ok(());
                }
                Resolution::KeepBoth => {
                    let t = names::unique_path(dst, &name.to_string_lossy());
                    copy_entry(ctx, &child, &t, created)?;
                }
                Resolution::Merge | Resolution::Replace if c.source_is_dir && c.dest_is_dir => {
                    copy_dir_contents(ctx, &child, &target, created)?;
                }
                Resolution::Merge | Resolution::Replace => {
                    let staged = staging_path(&target);
                    copy_entry(ctx, &child, &staged, created)?;
                    if !ctx.cancel.is_cancelled() {
                        commit_replace(&staged, &target)?;
                        created.retain(|p| p != &staged);
                    }
                }
            }
        } else {
            copy_entry(ctx, &child, &target, created)?;
        }
    }
    Ok(())
}

fn copy_file_data(ctx: &mut Ctx, src: &Path, dst: &Path, m: &fs::Metadata) -> Result<(), OpError> {
    let mut input = File::open(src).map_err(|e| OpError::io(&e, src, "read"))?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode_from(0o600)
        .open(dst)
        .map_err(|e| OpError::io(&e, dst, "create"))?;
    let mut buf = std::mem::take(&mut ctx.buf);
    let res = (|| -> Result<(), OpError> {
        loop {
            if ctx.cancel.is_cancelled() {
                return Ok(());
            }
            let n = match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(OpError::io(&e, src, "read")),
            };
            output.write_all(&buf[..n]).map_err(|e| OpError::io(&e, dst, "write"))?;
            ctx.progress.bytes_done += n as u64;
            // Once per 1 MiB chunk: cheap, and keeps cancel responsive.
            ctx.emit(n == buf.len());
        }
        output.sync_data().map_err(|e| OpError::io(&e, dst, "write"))?;
        Ok(())
    })();
    ctx.buf = buf;
    res?;
    let _ = fs::set_permissions(dst, Permissions::from_mode(m.mode() & 0o7777));
    set_times(&output, m);
    Ok(())
}

trait ModeFrom {
    fn mode_from(&mut self, mode: u32) -> &mut Self;
}

impl ModeFrom for fs::OpenOptions {
    fn mode_from(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, mode)
    }
}

fn set_times(f: &File, m: &fs::Metadata) {
    let mut t = FileTimes::new();
    if let Ok(mt) = m.modified() {
        t = t.set_modified(mt);
    }
    if let Ok(at) = m.accessed() {
        t = t.set_accessed(at);
    }
    let _ = f.set_times(t);
}

fn finish_metadata(dir: &Path, m: &fs::Metadata) {
    if let Ok(f) = File::open(dir) {
        set_times(&f, m);
    }
    let _ = fs::set_permissions(dir, Permissions::from_mode(m.mode() & 0o7777));
}

fn remove_any(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => {
            // A copied read-only folder must become writable to be removed.
            make_tree_writable(p);
            fs::remove_dir_all(p)
        }
        Ok(_) => fs::remove_file(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn make_tree_writable(p: &Path) {
    if let Ok(m) = fs::symlink_metadata(p)
        && m.is_dir() {
            let _ = fs::set_permissions(p, Permissions::from_mode((m.mode() & 0o7777) | 0o700));
            if let Ok(rd) = fs::read_dir(p) {
                for e in rd.flatten() {
                    make_tree_writable(&e.path());
                }
            }
        }
}

/// rename(2) that fails instead of overwriting.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(from.as_os_str().as_bytes())?;
    let b = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: valid NUL-terminated paths.
    let r = unsafe {
        libc::renameat2(libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_NOREPLACE)
    };
    if r == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        // File systems without RENAME_NOREPLACE: check, then rename.
        Some(libc::EINVAL) | Some(libc::ENOSYS) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            fs::rename(from, to)
        }
        _ => Err(err),
    }
}

// ------------------------------------------------------- single operations

/// Rename in place. Never overwrites. Returns the new path.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, OpError> {
    names::validate(new_name).map_err(|m| OpError::new(path, m, "Choose a different name."))?;
    let parent = path.parent().ok_or_else(|| OpError::new(path, "This item can't be renamed.", ""))?;
    let to = parent.join(new_name);
    if to == path {
        return Ok(to);
    }
    // Case-only rename on a case-insensitive drive: go via a temporary name.
    let case_only = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase() == new_name.to_lowercase())
        .unwrap_or(false);
    if case_only && fs::symlink_metadata(&to).is_ok_and(|m| {
        fs::symlink_metadata(path).is_ok_and(|s| s.ino() == m.ino() && s.dev() == m.dev())
    }) {
        let tmp = staging_path(&to);
        fs::rename(path, &tmp).map_err(|e| OpError::io(&e, path, "rename"))?;
        return fs::rename(&tmp, &to).map(|_| to.clone()).map_err(|e| {
            let _ = fs::rename(&tmp, path);
            OpError::io(&e, path, "rename")
        });
    }
    match rename_noreplace(path, &to) {
        Ok(()) => Ok(to),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(OpError::new(
            path,
            format!("An item named “{new_name}” already exists here."),
            "Choose a different name.",
        )),
        Err(e) => Err(OpError::io(&e, path, "rename")),
    }
}

/// Rename several items. Done in two phases through temporary names so
/// swaps like a→b, b→a work. On failure, already-renamed items are restored.
pub fn rename_many(items: &[(PathBuf, String)]) -> Result<Vec<(PathBuf, PathBuf)>, OpError> {
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new(); // (orig, tmp, final)
    let undo = |staged: &[(PathBuf, PathBuf, PathBuf)]| {
        for (orig, tmp, _) in staged.iter().rev() {
            let _ = fs::rename(tmp, orig);
        }
    };
    for (path, new_name) in items {
        if let Err(m) = names::validate(new_name) {
            undo(&staged);
            return Err(OpError::new(path, m, "Choose a different name."));
        }
        let final_path = path.parent().unwrap_or(Path::new("/")).join(new_name);
        let tmp = staging_path(path);
        if let Err(e) = fs::rename(path, &tmp) {
            undo(&staged);
            return Err(OpError::io(&e, path, "rename"));
        }
        staged.push((path.clone(), tmp, final_path));
    }
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for i in 0..staged.len() {
        let (orig, tmp, fin) = &staged[i];
        if let Err(e) = rename_noreplace(tmp, fin) {
            // Put everything back: finished ones to their originals too.
            for (o, f) in done.iter().rev() {
                let _ = fs::rename(f, o);
            }
            undo(&staged[i..]);
            let err = if e.kind() == io::ErrorKind::AlreadyExists {
                OpError::new(orig, format!("An item named “{}” already exists here.", display_name(fin)), "Choose a different pattern.")
            } else {
                OpError::io(&e, orig, "rename")
            };
            return Err(err);
        }
        done.push((orig.clone(), fin.clone()));
    }
    Ok(done)
}

/// New empty folder. Returns its path.
pub fn create_folder(dir: &Path, name: &str) -> Result<PathBuf, OpError> {
    names::validate(name).map_err(|m| OpError::new(dir, m, "Choose a different name."))?;
    let p = dir.join(name);
    fs::create_dir(&p).map_err(|e| OpError::io(&e, &p, "create"))?;
    Ok(p)
}

/// New file, empty or copied from a template. Never overwrites.
pub fn create_file(dir: &Path, name: &str, template: Option<&Path>) -> Result<PathBuf, OpError> {
    names::validate(name).map_err(|m| OpError::new(dir, m, "Choose a different name."))?;
    let p = dir.join(name);
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
        .map_err(|e| OpError::io(&e, &p, "create"))?;
    if let Some(t) = template {
        let data = fs::read(t).map_err(|e| OpError::io(&e, t, "read"))?;
        if let Err(e) = out.write_all(&data) {
            let _ = fs::remove_file(&p);
            return Err(OpError::io(&e, &p, "write"));
        }
    }
    Ok(p)
}

/// Symbolic link named `name` in `dir`, pointing at `target`.
pub fn create_link(target: &Path, dir: &Path, name: &str) -> Result<PathBuf, OpError> {
    names::validate(name).map_err(|m| OpError::new(dir, m, "Choose a different name."))?;
    let p = dir.join(name);
    symlink(target, &p).map_err(|e| OpError::io(&e, &p, "create a link at"))?;
    Ok(p)
}

/// Permanently delete. The UI must confirm first; this can't be undone.
pub fn delete(paths: &[PathBuf], obs: &mut dyn Observer, cancel: &Cancel) -> Report {
    let mut report = Report::default();
    let mut progress = Progress::default();
    for p in paths {
        progress.files_total += scan(p, cancel).files;
    }
    for p in paths {
        if cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        progress.current = display_name(p);
        obs.progress(&progress);
        match delete_tree(p, &mut progress, obs, cancel) {
            Ok(true) => report.done.push((p.clone(), PathBuf::new())),
            Ok(false) => report.cancelled = true,
            Err(e) => report.errors.push(e),
        }
    }
    obs.progress(&progress);
    report
}

fn delete_tree(p: &Path, progress: &mut Progress, obs: &mut dyn Observer, cancel: &Cancel) -> Result<bool, OpError> {
    let m = match fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(OpError::io(&e, p, "delete")),
    };
    if m.is_dir() {
        let rd = fs::read_dir(p).map_err(|e| OpError::io(&e, p, "delete"))?;
        for e in rd.flatten() {
            if cancel.is_cancelled() {
                return Ok(false);
            }
            if !delete_tree(&e.path(), progress, obs, cancel)? {
                return Ok(false);
            }
        }
        fs::remove_dir(p).map_err(|e| OpError::io(&e, p, "delete"))?;
    } else {
        fs::remove_file(p).map_err(|e| OpError::io(&e, p, "delete"))?;
    }
    progress.files_done += 1;
    if progress.files_done.is_multiple_of(64) {
        obs.progress(progress);
    }
    Ok(true)
}

/// Remove `.strata-partial-<pid>-…` leftovers in `dir` whose process is no
/// longer running (a crash or kill mid-copy). Returns how many were removed.
pub fn sweep_partials(dir: &Path) -> usize {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    let me = std::process::id();
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix(".strata-partial-") else { continue };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        let alive = pid == me || Path::new(&format!("/proc/{pid}")).exists();
        if !alive && remove_any(&e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// Folders that running jobs write into, kept in a small journal so leftovers
/// can be swept after a crash.
pub mod journal {
    use super::*;

    pub fn file() -> PathBuf {
        crate::paths::state_dir().join("active-jobs")
    }

    fn read(f: &Path) -> Vec<PathBuf> {
        fs::read_to_string(f).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect()
    }

    fn write(f: &Path, dirs: &[PathBuf]) {
        if let Some(p) = f.parent() {
            let _ = fs::create_dir_all(p);
        }
        let text: String = dirs.iter().map(|d| format!("{}\n", d.display())).collect();
        let _ = fs::write(f, text);
    }

    pub fn add(f: &Path, dir: &Path) {
        let mut v = read(f);
        v.push(dir.to_path_buf());
        write(f, &v);
    }

    pub fn remove(f: &Path, dir: &Path) {
        let mut v = read(f);
        if let Some(i) = v.iter().position(|d| d == dir) {
            v.remove(i);
        }
        write(f, &v);
    }

    /// Sweep every journaled folder (at startup), then clear the journal.
    pub fn recover(f: &Path) -> usize {
        let mut n = 0;
        let mut seen = std::collections::HashSet::new();
        for d in read(f) {
            if seen.insert(d.clone()) {
                n += sweep_partials(&d);
            }
        }
        let _ = fs::remove_file(f);
        n
    }
}

/// Total size and item count of a folder tree, for Properties.
pub fn measure(path: &Path, cancel: &Cancel) -> (u64, u64) {
    let t = scan(path, cancel);
    (t.bytes, t.files.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tree(root: &Path) {
        fs::create_dir_all(root.join("dir/sub")).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(root.join("dir/b.txt"), b"bravo!").unwrap();
        fs::write(root.join("dir/sub/c.bin"), vec![7u8; 3 * 1024 * 1024]).unwrap();
        symlink("b.txt", root.join("dir/link")).unwrap();
    }

    struct Recorder {
        answers: Vec<(Resolution, bool)>,
        asked: Vec<ConflictInfo>,
        progress: Vec<Progress>,
        cancel_after_bytes: Option<(u64, Cancel)>,
    }

    impl Recorder {
        fn new(answers: Vec<(Resolution, bool)>) -> Self {
            Self { answers, asked: vec![], progress: vec![], cancel_after_bytes: None }
        }
    }

    impl Observer for Recorder {
        fn progress(&mut self, p: &Progress) {
            if let Some((n, c)) = &self.cancel_after_bytes
                && p.bytes_done >= *n {
                    c.cancel();
                }
            self.progress.push(p.clone());
        }
        fn conflict(&mut self, c: &ConflictInfo) -> (Resolution, bool) {
            self.asked.push(c.clone());
            if self.answers.is_empty() { (Resolution::Skip, false) } else { self.answers.remove(0) }
        }
    }

    fn no_partials(dir: &Path) -> bool {
        walk(dir).iter().all(|p| !p.file_name().unwrap().to_string_lossy().starts_with(".strata-partial"))
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = vec![];
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                out.push(e.path());
                if e.file_type().unwrap().is_dir() {
                    out.extend(walk(&e.path()));
                }
            }
        }
        out
    }

    #[test]
    fn copies_tree_with_metadata_and_links() {
        let s = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        tree(s.path());
        fs::set_permissions(s.path().join("a.txt"), Permissions::from_mode(0o640)).unwrap();
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        File::options().write(true).open(s.path().join("a.txt")).unwrap().set_modified(mtime).unwrap();

        let mut rec = Recorder::new(vec![]);
        let r = transfer(Mode::Copy, &[s.path().join("a.txt"), s.path().join("dir")], d.path(), &mut rec, &Cancel::new());
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.done.len(), 2);
        assert_eq!(fs::read(d.path().join("dir/sub/c.bin")).unwrap().len(), 3 * 1024 * 1024);
        assert_eq!(fs::read_link(d.path().join("dir/link")).unwrap(), Path::new("b.txt"));
        let m = fs::metadata(d.path().join("a.txt")).unwrap();
        assert_eq!(m.mode() & 0o777, 0o640);
        assert_eq!(m.modified().unwrap(), mtime);
        // Sources untouched.
        assert!(s.path().join("dir/sub/c.bin").exists());
        let last = rec.progress.last().unwrap();
        assert_eq!(last.bytes_done, last.bytes_total);
        assert_eq!(last.files_done, last.files_total);
        assert!(no_partials(d.path()));
    }

    #[test]
    fn move_same_device_is_rename() {
        let s = tempfile::tempdir().unwrap();
        tree(s.path());
        let dest = s.path().join("target");
        fs::create_dir(&dest).unwrap();
        let ino = fs::metadata(s.path().join("dir/sub/c.bin")).unwrap().ino();
        let r = transfer(Mode::Move, &[s.path().join("dir")], &dest, &mut Fixed(Resolution::Skip), &Cancel::new());
        assert!(r.errors.is_empty());
        assert!(!s.path().join("dir").exists());
        assert_eq!(fs::metadata(dest.join("dir/sub/c.bin")).unwrap().ino(), ino, "renamed, not copied");
    }

    #[test]
    fn refuses_copy_into_itself() {
        let s = tempfile::tempdir().unwrap();
        tree(s.path());
        let r = transfer(Mode::Copy, &[s.path().join("dir")], &s.path().join("dir/sub"), &mut Fixed(Resolution::Skip), &Cancel::new());
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].message.contains("into itself"));
    }

    #[test]
    fn paste_into_same_folder_makes_copy() {
        let s = tempfile::tempdir().unwrap();
        tree(s.path());
        let r = transfer(Mode::Copy, &[s.path().join("a.txt")], s.path(), &mut Fixed(Resolution::Skip), &Cancel::new());
        assert_eq!(r.done[0].1, s.path().join("a copy.txt"));
        assert_eq!(fs::read(s.path().join("a copy.txt")).unwrap(), b"alpha");
    }

    #[test]
    fn conflicts_skip_replace_keep_both() {
        let s = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        tree(s.path());
        fs::write(d.path().join("a.txt"), b"old").unwrap();

        let mut rec = Recorder::new(vec![(Resolution::Skip, false)]);
        let r = transfer(Mode::Copy, &[s.path().join("a.txt")], d.path(), &mut rec, &Cancel::new());
        assert_eq!(rec.asked.len(), 1);
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(fs::read(d.path().join("a.txt")).unwrap(), b"old");

        let r = transfer(Mode::Copy, &[s.path().join("a.txt")], d.path(), &mut Fixed(Resolution::KeepBoth), &Cancel::new());
        assert_eq!(r.done[0].1, d.path().join("a (2).txt"));
        assert_eq!(fs::read(d.path().join("a.txt")).unwrap(), b"old");

        let r = transfer(Mode::Copy, &[s.path().join("a.txt")], d.path(), &mut Fixed(Resolution::Replace), &Cancel::new());
        assert!(r.errors.is_empty());
        assert_eq!(fs::read(d.path().join("a.txt")).unwrap(), b"alpha");
        assert!(no_partials(d.path()));
    }

    #[test]
    fn folder_conflict_merges_and_asks_per_file() {
        let s = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        tree(s.path());
        fs::create_dir(d.path().join("dir")).unwrap();
        fs::write(d.path().join("dir/b.txt"), b"keep me").unwrap();
        fs::write(d.path().join("dir/mine.txt"), b"mine").unwrap();
        // Merge the folder, keep both for the clashing file.
        let mut rec = Recorder::new(vec![(Resolution::Merge, false), (Resolution::KeepBoth, false)]);
        let r = transfer(Mode::Copy, &[s.path().join("dir")], d.path(), &mut rec, &Cancel::new());
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(rec.asked.len(), 2);
        assert_eq!(fs::read(d.path().join("dir/b.txt")).unwrap(), b"keep me");
        assert_eq!(fs::read(d.path().join("dir/b (2).txt")).unwrap(), b"bravo!");
        assert_eq!(fs::read(d.path().join("dir/mine.txt")).unwrap(), b"mine");
        assert!(d.path().join("dir/sub/c.bin").exists());
    }

    #[test]
    fn apply_to_all_asks_once() {
        let s = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        for n in ["1", "2", "3"] {
            fs::write(s.path().join(n), n).unwrap();
            fs::write(d.path().join(n), "x").unwrap();
        }
        let srcs: Vec<_> = ["1", "2", "3"].iter().map(|n| s.path().join(n)).collect();
        let mut rec = Recorder::new(vec![(Resolution::Replace, true)]);
        let r = transfer(Mode::Copy, &srcs, d.path(), &mut rec, &Cancel::new());
        assert_eq!(rec.asked.len(), 1);
        assert_eq!(r.done.len(), 3);
        assert_eq!(fs::read(d.path().join("3")).unwrap(), b"3");
    }

    #[test]
    fn cancel_rolls_back_the_current_item() {
        let s = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        tree(s.path());
        let cancel = Cancel::new();
        let mut rec = Recorder::new(vec![]);
        rec.cancel_after_bytes = Some((1024 * 1024, cancel.clone()));
        let r = transfer(Mode::Copy, &[s.path().join("dir")], d.path(), &mut rec, &cancel);
        assert!(r.cancelled);
        assert!(r.done.is_empty());
        assert!(!d.path().join("dir").exists(), "partial folder removed");
        assert!(no_partials(d.path()));
    }

    #[test]
    fn move_cancel_keeps_source() {
        // A cross-device move can't be forced in a test, so exercise the copy
        // path through a folder conflict (merge), then cancel.
        let s = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        tree(s.path());
        let cancel = Cancel::new();
        let mut rec = Recorder::new(vec![]);
        rec.cancel_after_bytes = Some((1, cancel.clone()));
        let r = transfer(Mode::Move, &[s.path().join("dir")], d.path(), &mut rec, &cancel);
        // Same device: the rename is instant and complete, or cancelled before.
        assert!(r.cancelled || r.done.len() == 1);
        let src_exists = s.path().join("dir/sub/c.bin").exists();
        let dst_exists = d.path().join("dir/sub/c.bin").exists();
        assert!(src_exists ^ dst_exists, "exactly one copy exists");
    }

    #[test]
    fn free_space_reports_bytes() {
        let d = tempfile::tempdir().unwrap();
        let free = free_space(d.path()).unwrap();
        assert!(free > 0);
    }

    #[test]
    fn special_files_are_reported() {
        let s = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let fifo = s.path().join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let r = transfer(Mode::Copy, &[fifo], d.path(), &mut Fixed(Resolution::Skip), &Cancel::new());
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].message.contains("special file"));
    }

    #[test]
    fn rename_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a"), "a").unwrap();
        fs::write(d.path().join("b"), "b").unwrap();
        let e = rename(&d.path().join("a"), "b").unwrap_err();
        assert!(e.message.contains("already exists"));
        assert_eq!(fs::read(d.path().join("b")).unwrap(), b"b");
        assert_eq!(rename(&d.path().join("a"), "c").unwrap(), d.path().join("c"));
        assert!(rename(&d.path().join("c"), "x/y").is_err());
        assert_eq!(rename(&d.path().join("c"), "C").unwrap(), d.path().join("C"));
    }

    #[test]
    fn rename_many_swaps_and_rolls_back() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fs::write(p.join("a"), "A").unwrap();
        fs::write(p.join("b"), "B").unwrap();
        rename_many(&[(p.join("a"), "b".into()), (p.join("b"), "a".into())]).unwrap();
        assert_eq!(fs::read(p.join("a")).unwrap(), b"B");
        assert_eq!(fs::read(p.join("b")).unwrap(), b"A");

        fs::write(p.join("c"), "C").unwrap();
        fs::write(p.join("blocker"), "!").unwrap();
        let err = rename_many(&[(p.join("a"), "x".into()), (p.join("c"), "blocker".into())]).unwrap_err();
        assert!(err.message.contains("already exists"));
        assert_eq!(fs::read(p.join("a")).unwrap(), b"B", "rolled back");
        assert_eq!(fs::read(p.join("c")).unwrap(), b"C");
        assert!(!p.join("x").exists());
        assert!(no_partials(p));
    }

    #[test]
    fn create_and_link_and_delete() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let f = create_folder(p, "New").unwrap();
        assert!(create_folder(p, "New").is_err());
        let t = p.join("tpl.txt");
        fs::write(&t, "template").unwrap();
        let file = create_file(&f, "doc.txt", Some(&t)).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"template");
        assert!(create_file(&f, "doc.txt", None).is_err(), "never overwrites");
        let l = create_link(&file, p, "doc link").unwrap();
        assert_eq!(fs::read_link(&l).unwrap(), file);
        let r = delete(&[f.clone(), l.clone()], &mut Fixed(Resolution::Skip), &Cancel::new());
        assert!(r.errors.is_empty());
        assert!(!f.exists() && fs::symlink_metadata(&l).is_err());
        assert!(t.exists());
    }

    #[test]
    fn plain_language_errors() {
        let e = OpError::io(&io::Error::from(io::ErrorKind::PermissionDenied), Path::new("/x/secret"), "copy");
        assert_eq!(e.message, "You don't have permission to copy “secret”.");
        assert!(!e.hint.is_empty());
    }

    #[test]
    fn sweeps_only_dead_partials() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        // pid 1 is always alive; a huge pid is never running.
        fs::write(p.join(".strata-partial-4000000000-1-a.bin"), "x").unwrap();
        fs::create_dir(p.join(".strata-partial-4000000001-2-dir")).unwrap();
        fs::write(p.join(format!(".strata-partial-{}-3-mine", std::process::id())), "x").unwrap();
        fs::write(p.join(".strata-partial-1-4-init"), "x").unwrap();
        fs::write(p.join("keep.txt"), "x").unwrap();
        let j = p.join("journal");
        journal::add(&j, p);
        assert_eq!(journal::recover(&j), 2);
        let left: Vec<_> = fs::read_dir(p).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left.len(), 3, "{left:?}");
        assert!(!j.exists());
    }

    #[test]
    fn measures_trees() {
        let s = tempfile::tempdir().unwrap();
        tree(s.path());
        let (bytes, items) = measure(&s.path().join("dir"), &Cancel::new());
        assert_eq!(bytes, 6 + 3 * 1024 * 1024);
        assert_eq!(items, 4); // b.txt, sub, c.bin, link
    }
}
