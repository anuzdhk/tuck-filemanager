// SPDX-License-Identifier: GPL-3.0-or-later
//! Checking for and downloading new versions of Tuck from the project's
//! GitHub releases.
//!
//! Nothing is installed silently: the UI shows what's new, downloads the
//! package that fits this system (deb, rpm, Arch package or AppImage) into
//! the Downloads folder, checks it against the release's SHA-256 checksums
//! when they're published, and then hands it to the normal install flow.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::install::OsFamily;
use crate::ops::Cancel;

/// GitHub repository the releases come from.
pub const REPO: &str = "anuzdhk/kivo-filemanager";

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Release {
    #[serde(rename = "tag_name")]
    pub tag: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Release notes (Markdown).
    #[serde(default)]
    pub body: Option<String>,
    #[serde(rename = "html_url", default)]
    pub page: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    /// "1.2.0" from "v1.2.0".
    pub fn version(&self) -> &str {
        self.tag.trim_start_matches(['v', 'V'])
    }
}

/// Parse "1.2.3" (optionally "v1.2.3", "1.2", "1.2.3-beta.1") into numbers.
/// A pre-release sorts before the plain version.
fn parse_version(v: &str) -> Option<(Vec<u64>, bool)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let (main, pre) = match v.split_once(['-', '+']) {
        Some((m, rest)) => (m, !rest.is_empty() && v.contains('-')),
        None => (v, false),
    };
    let mut nums: Vec<u64> = main.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    while nums.len() < 3 {
        nums.push(0);
    }
    Some((nums, pre))
}

/// Is `latest` newer than `current`?
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some((l, lpre)), Some((c, cpre))) => match l.cmp(&c) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            // Same numbers: only a final release beats a pre-release.
            std::cmp::Ordering::Equal => cpre && !lpre,
        },
        _ => false,
    }
}

/// Which kind of package suits this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    Deb,
    Rpm,
    Arch,
    AppImage,
}

impl PackageKind {
    pub fn label(self) -> &'static str {
        match self {
            PackageKind::Deb => "Debian/Ubuntu package (.deb)",
            PackageKind::Rpm => "RPM package (.rpm)",
            PackageKind::Arch => "Arch package (.pkg.tar.zst)",
            PackageKind::AppImage => "AppImage",
        }
    }

    fn matches(self, name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        match self {
            PackageKind::Deb => n.ends_with(".deb"),
            PackageKind::Rpm => n.ends_with(".rpm") && !n.ends_with(".src.rpm"),
            PackageKind::Arch => n.ends_with(".pkg.tar.zst") || n.ends_with(".pkg.tar.xz"),
            PackageKind::AppImage => n.ends_with(".appimage"),
        }
    }
}

/// The package kind for this system. Running from an AppImage updates the
/// AppImage; otherwise the distro family decides, with AppImage as fallback.
pub fn package_kind(family: OsFamily, running_appimage: bool) -> PackageKind {
    if running_appimage {
        return PackageKind::AppImage;
    }
    match family {
        OsFamily::Debian => PackageKind::Deb,
        OsFamily::Fedora | OsFamily::Suse => PackageKind::Rpm,
        // Arch users who installed the package update it with pacman; the
        // install flow can't install Arch packages, so offer the AppImage.
        OsFamily::Arch | OsFamily::Unknown => PackageKind::AppImage,
    }
}

fn arch_words(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["x86_64", "amd64", "x64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[],
    }
}

/// The asset to download: right kind and CPU architecture. Falls back to an
/// AppImage when there's no package of the preferred kind.
pub fn pick_asset<'a>(release: &'a Release, kind: PackageKind, arch: &str) -> Option<(&'a Asset, PackageKind)> {
    let all_arches = ["x86_64", "amd64", "x64", "aarch64", "arm64", "i686", "i386", "armhf", "armv7"];
    let fits = |a: &Asset| {
        let n = a.name.to_ascii_lowercase();
        let mine = arch_words(arch);
        // Named for this CPU, or not named for any CPU (e.g. "noarch").
        mine.iter().any(|w| n.contains(w)) || !all_arches.iter().any(|w| n.contains(w))
    };
    for k in [kind, PackageKind::AppImage] {
        if let Some(a) = release.assets.iter().find(|a| k.matches(&a.name) && fits(a)) {
            return Some((a, k));
        }
    }
    None
}

/// The checksum for `name` from a `SHA256SUMS`-style file
/// (`<hex>  <name>` per line, `*name` for binary mode).
pub fn checksum_from_sums(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let hash = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
    })
}

/// The checksums asset for `asset`, if the release has one.
pub fn checksum_asset<'a>(release: &'a Release, asset: &str) -> Option<&'a Asset> {
    let own = format!("{asset}.sha256");
    release.assets.iter().find(|a| a.name == own).or_else(|| {
        release.assets.iter().find(|a| {
            let n = a.name.to_ascii_lowercase();
            n == "sha256sums" || n == "sha256sums.txt" || n == "checksums.txt" || n.ends_with(".sha256sums")
        })
    })
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(timeout)
        .user_agent(format!("Tuck/{CURRENT} (file manager update check)"))
        .build()
        .into()
}

fn get_text(url: &str) -> Result<String, String> {
    let mut resp = agent(Some(Duration::from_secs(30)))
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| friendly(&e))?;
    resp.body_mut().read_to_string().map_err(|e| friendly(&e))
}

fn friendly(e: &ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(404) => "No releases have been published yet.".into(),
        ureq::Error::StatusCode(403) | ureq::Error::StatusCode(429) => "GitHub is limiting requests right now. Try again in a little while.".into(),
        ureq::Error::StatusCode(c) => format!("The update server answered with an error ({c})."),
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => "Couldn't reach the update server. Check your internet connection.".into(),
        ureq::Error::Timeout(_) => "The update server took too long to answer.".into(),
        other => format!("Couldn't check for updates: {other}"),
    }
}

/// The newest published release (drafts and pre-releases excluded).
pub fn latest() -> Result<Release, String> {
    // TUCK_UPDATE_URL: a different release feed, for testing.
    let url = std::env::var("TUCK_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let text = get_text(&url)?;
    serde_json::from_str(&text).map_err(|e| format!("The update information couldn't be read: {e}"))
}

/// Download `asset` into `dir`, verifying it when the release publishes a
/// checksum. Writes to a hidden partial file and renames it into place under
/// a name that never overwrites anything. `progress(done, total)`.
pub fn download(
    release: &Release,
    asset: &Asset,
    dir: &Path,
    cancel: &Cancel,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(PathBuf, bool), String> {
    let expected = match checksum_asset(release, &asset.name) {
        Some(sums) => {
            let text = get_text(&sums.url)?;
            Some(checksum_from_sums(&text, &asset.name).or_else(|| {
                // A bare "<hex>" file for this one asset.
                let t = text.trim();
                (t.len() >= 64 && t[..64].chars().all(|c| c.is_ascii_hexdigit())).then(|| t[..64].to_ascii_lowercase())
            }).ok_or_else(|| "The release's checksum file doesn't list this download.".to_string())?)
        }
        None => None,
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    let part = dir.join(format!(".{}.part", asset.name));
    let result = (|| {
        let mut resp = agent(None).get(&asset.url).call().map_err(|e| friendly(&e))?;
        let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(asset.size);
        let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
        let mut file = std::fs::File::create(&part).map_err(|e| format!("Couldn't save the download: {e}"))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut done = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err("Cancelled".to_string());
            }
            let n = reader.read(&mut buf).map_err(|e| format!("The download was interrupted: {e}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| format!("Couldn't save the download: {e}"))?;
            hasher.update(&buf[..n]);
            done += n as u64;
            progress(done, total);
        }
        file.sync_all().map_err(|e| format!("Couldn't save the download: {e}"))?;
        if total > 0 && done != total {
            return Err("The download ended early. Please try again.".into());
        }
        let got = hex(&hasher.finalize());
        if let Some(want) = &expected
            && *want != got
        {
            return Err("The download doesn't match its published checksum, so it was discarded.".into());
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let dest = crate::names::unique_path(dir, &asset.name);
    std::fs::rename(&part, &dest).map_err(|e| format!("Couldn't save the download: {e}"))?;
    if asset.name.to_ascii_lowercase().ends_with(".appimage") {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }
    Ok((dest, expected.is_some()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{
        "tag_name": "v0.2.0", "name": "Tuck 0.2", "body": "Faster search.",
        "html_url": "https://github.com/x/y/releases/tag/v0.2.0", "draft": false,
        "assets": [
            {"name": "tuck_0.2.0_amd64.deb", "browser_download_url": "https://e/deb", "size": 10},
            {"name": "tuck_0.2.0_arm64.deb", "browser_download_url": "https://e/deb-arm", "size": 10},
            {"name": "tuck-0.2.0-1.x86_64.rpm", "browser_download_url": "https://e/rpm", "size": 11},
            {"name": "tuck-0.2.0-1.src.rpm", "browser_download_url": "https://e/srpm", "size": 11},
            {"name": "Tuck-0.2.0-x86_64.AppImage", "browser_download_url": "https://e/ai", "size": 12},
            {"name": "SHA256SUMS", "browser_download_url": "https://e/sums", "size": 1}
        ]
    }"#;

    #[test]
    fn versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.0"), "numeric, not text order");
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(is_newer("1.0.0", "1.0.0-beta.2"));
        assert!(!is_newer("1.0.0-rc.1", "1.0.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    #[test]
    fn parses_release_and_picks_assets() {
        let r: Release = serde_json::from_str(JSON).expect("json");
        assert_eq!(r.version(), "0.2.0");
        let pick = |k, a| pick_asset(&r, k, a).map(|(x, k)| (x.name.clone(), k));
        assert_eq!(pick(PackageKind::Deb, "x86_64"), Some(("tuck_0.2.0_amd64.deb".into(), PackageKind::Deb)));
        assert_eq!(pick(PackageKind::Deb, "aarch64"), Some(("tuck_0.2.0_arm64.deb".into(), PackageKind::Deb)));
        assert_eq!(pick(PackageKind::Rpm, "x86_64"), Some(("tuck-0.2.0-1.x86_64.rpm".into(), PackageKind::Rpm)));
        // No Arch package published: fall back to the AppImage.
        assert_eq!(pick(PackageKind::Arch, "x86_64"), Some(("Tuck-0.2.0-x86_64.AppImage".into(), PackageKind::AppImage)));
        // Nothing for this CPU at all.
        assert_eq!(pick(PackageKind::Rpm, "aarch64"), None);
        assert_eq!(checksum_asset(&r, "tuck_0.2.0_amd64.deb").map(|a| a.name.as_str()), Some("SHA256SUMS"));
    }

    #[test]
    fn package_kinds() {
        assert_eq!(package_kind(OsFamily::Debian, false), PackageKind::Deb);
        assert_eq!(package_kind(OsFamily::Suse, false), PackageKind::Rpm);
        assert_eq!(package_kind(OsFamily::Arch, false), PackageKind::AppImage);
        assert_eq!(package_kind(OsFamily::Unknown, false), PackageKind::AppImage);
        assert_eq!(package_kind(OsFamily::Debian, true), PackageKind::AppImage);
    }

    #[test]
    fn checksums() {
        let h = "a".repeat(64);
        let sums = format!("{h}  tuck.deb\n{}  *Tuck.AppImage\n", "B".repeat(64));
        assert_eq!(checksum_from_sums(&sums, "tuck.deb"), Some(h));
        assert_eq!(checksum_from_sums(&sums, "Tuck.AppImage"), Some("b".repeat(64)));
        assert_eq!(checksum_from_sums(&sums, "other"), None);
        assert_eq!(hex(&Sha256::digest(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
