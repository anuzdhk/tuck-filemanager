// SPDX-License-Identifier: GPL-3.0-or-later
//! Unix permissions in the terms the Properties dialog uses: who (owner,
//! group, others) gets what access (none, read, read & write), plus execute.

use std::ffi::CStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Owner,
    Group,
    Others,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    None,
    Read,
    ReadWrite,
    /// Write without read: unusual, shown as-is and never produced by us.
    WriteOnly,
}

impl Access {
    pub const CHOICES: [Access; 3] = [Access::None, Access::Read, Access::ReadWrite];

    pub fn label(self, is_dir: bool) -> &'static str {
        match (self, is_dir) {
            (Access::None, _) => "No Access",
            (Access::Read, false) => "Read Only",
            (Access::Read, true) => "View Contents",
            (Access::ReadWrite, false) => "Read and Write",
            (Access::ReadWrite, true) => "View and Modify Contents",
            (Access::WriteOnly, _) => "Write Only",
        }
    }
}

fn shift(who: Who) -> u32 {
    match who {
        Who::Owner => 6,
        Who::Group => 3,
        Who::Others => 0,
    }
}

pub fn access(mode: u32, who: Who) -> Access {
    let bits = (mode >> shift(who)) & 0o7;
    match (bits & 4 != 0, bits & 2 != 0) {
        (false, false) => Access::None,
        (true, false) => Access::Read,
        (true, true) => Access::ReadWrite,
        (false, true) => Access::WriteOnly,
    }
}

/// Set `who`'s access. For folders, read access also grants "enter" (x),
/// because a folder you can list but not enter is useless.
pub fn with_access(mode: u32, who: Who, a: Access, is_dir: bool) -> u32 {
    let s = shift(who);
    let mut bits = (mode >> s) & 0o7;
    bits &= !0o6;
    match a {
        Access::None => {
            if is_dir {
                bits &= !0o1;
            }
        }
        Access::Read => {
            bits |= 0o4;
            if is_dir {
                bits |= 0o1;
            }
        }
        Access::ReadWrite => {
            bits |= 0o6;
            if is_dir {
                bits |= 0o1;
            }
        }
        Access::WriteOnly => bits |= 0o2,
    }
    (mode & !(0o7 << s)) | (bits << s)
}

pub fn is_executable(mode: u32) -> bool {
    mode & 0o111 != 0
}

/// Make a file executable for everyone who can read it, or not executable.
pub fn with_executable(mode: u32, exec: bool) -> u32 {
    if !exec {
        return mode & !0o111;
    }
    let mut m = mode;
    for who in [Who::Owner, Who::Group, Who::Others] {
        if (m >> shift(who)) & 0o4 != 0 {
            m |= 0o1 << shift(who);
        }
    }
    m
}

/// "rwxr-xr--" style, with setuid/setgid/sticky shown like `ls`.
pub fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for (i, who) in [Who::Owner, Who::Group, Who::Others].into_iter().enumerate() {
        let b = (mode >> shift(who)) & 0o7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        let special = match i {
            0 => mode & 0o4000 != 0,
            1 => mode & 0o2000 != 0,
            _ => mode & 0o1000 != 0,
        };
        let x = b & 1 != 0;
        s.push(match (special, x, i) {
            (true, true, 2) => 't',
            (true, false, 2) => 'T',
            (true, true, _) => 's',
            (true, false, _) => 'S',
            (false, true, _) => 'x',
            (false, false, _) => '-',
        });
    }
    s
}

pub fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out = std::ptr::null_mut();
    // SAFETY: buffers are valid for the call; result checked before use.
    let r = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut out) };
    if r != 0 || out.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(pwd.pw_name) }.to_string_lossy().into_owned())
}

pub fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut out = std::ptr::null_mut();
    // SAFETY: as above.
    let r = unsafe { libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut out) };
    if r != 0 || out.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(grp.gr_name) }.to_string_lossy().into_owned())
}

pub fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_access() {
        assert_eq!(access(0o640, Who::Owner), Access::ReadWrite);
        assert_eq!(access(0o640, Who::Group), Access::Read);
        assert_eq!(access(0o640, Who::Others), Access::None);
        assert_eq!(access(0o020, Who::Group), Access::WriteOnly);
    }

    #[test]
    fn sets_access_keeping_other_bits() {
        assert_eq!(with_access(0o644, Who::Others, Access::None, false), 0o640);
        assert_eq!(with_access(0o4755, Who::Group, Access::ReadWrite, false), 0o4775);
        // Folders get x with read.
        assert_eq!(with_access(0o700, Who::Others, Access::Read, true), 0o705);
        assert_eq!(with_access(0o755, Who::Others, Access::None, true), 0o750);
        // Files keep their x bit as is.
        assert_eq!(with_access(0o755, Who::Others, Access::None, false), 0o751);
    }

    #[test]
    fn executable_bits() {
        assert!(is_executable(0o744));
        assert_eq!(with_executable(0o644, true), 0o755);
        assert_eq!(with_executable(0o640, true), 0o750);
        assert_eq!(with_executable(0o755, false), 0o644);
    }

    #[test]
    fn strings() {
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o4755), "rwsr-xr-x");
        assert_eq!(mode_string(0o1777), "rwxrwxrwt");
        assert_eq!(mode_string(0o640), "rw-r-----");
    }

    #[test]
    fn names() {
        assert_eq!(user_name(0).as_deref(), Some("root"));
        assert_eq!(group_name(0).as_deref(), Some("root"));
        assert!(user_name(current_uid()).is_some());
    }
}
