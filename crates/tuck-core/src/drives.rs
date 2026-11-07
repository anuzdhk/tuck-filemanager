// SPDX-License-Identifier: GPL-3.0-or-later
//! Partitions worth showing as drives: real disks mounted outside the
//! system's own folders, such as a separate /home or a /data disk. GIO's
//! volume monitor hides partitions mounted from /etc/fstab, so this reads
//! the mount table directly.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub device: String,
    pub mount: PathBuf,
}

/// System areas that aren't the user's storage.
const SYSTEM: &[&str] = &["/boot", "/efi", "/snap", "/var", "/usr", "/opt", "/tmp", "/proc", "/sys", "/dev", "/run", "/recovery"];

/// `/proc/mounts` escapes spaces and a few other bytes as `\NNN` (octal).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a mount table (`/proc/self/mounts`) into the user's partitions,
/// excluding `/` (shown as Computer) and anything on the same disk as `/`.
pub fn user_partitions(table: &str) -> Vec<Partition> {
    let mut root_dev = None;
    let mut out: Vec<Partition> = Vec::new();
    for line in table.lines() {
        let mut f = line.split_whitespace();
        let (Some(dev), Some(mnt)) = (f.next(), f.next()) else { continue };
        let mount = PathBuf::from(unescape(mnt));
        if !dev.starts_with("/dev/") || dev.starts_with("/dev/loop") {
            continue;
        }
        if mount == Path::new("/") {
            root_dev = Some(dev.to_string());
            continue;
        }
        // /run/media holds removable disks, which GIO already shows.
        if SYSTEM.iter().any(|s| mount.starts_with(s)) {
            continue;
        }
        if out.iter().any(|p| p.device == dev) {
            continue;
        }
        out.push(Partition { device: dev.to_string(), mount });
    }
    out.retain(|p| Some(&p.device) != root_dev.as_ref());
    out
}

/// The user's partitions on this machine.
pub fn list() -> Vec<Partition> {
    std::fs::read_to_string("/proc/self/mounts").map(|t| user_partitions(&t)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_separate_home_and_data_disk() {
        let table = "\
/dev/nvme0n1p2 / ext4 rw 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
/dev/loop0 /snap/core24/2124 squashfs ro 0 0
tmpfs /tmp tmpfs rw 0 0
/dev/nvme0n1p3 /home ext4 rw 0 0
/dev/sdb1 /mnt/My\\040Data ext4 rw 0 0
/dev/sdc1 /run/media/u/USB vfat rw 0 0
/dev/sdb1 /srv/again ext4 rw 0 0
";
        let p = user_partitions(table);
        assert_eq!(
            p,
            vec![
                Partition { device: "/dev/nvme0n1p3".into(), mount: "/home".into() },
                Partition { device: "/dev/sdb1".into(), mount: "/mnt/My Data".into() },
            ]
        );
    }

    #[test]
    fn btrfs_subvolumes_on_the_system_disk_are_not_separate_drives() {
        let table = "/dev/nvme0n1p2 / btrfs rw,subvol=/@ 0 0\n/dev/nvme0n1p2 /home btrfs rw,subvol=/@home 0 0\n";
        assert!(user_partitions(table).is_empty());
    }
}
