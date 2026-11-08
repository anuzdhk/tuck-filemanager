// SPDX-License-Identifier: GPL-3.0-or-later
//! Media addons: convert or compress images with ImageMagick, compress
//! videos with FFmpeg. Results are written next to the originals under a
//! free name; originals are never touched.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::names;
use crate::ops::{Cancel, Observer, OpError, Progress};

/// What a conversion produced.
#[derive(Debug, Default)]
pub struct Outcome {
    pub made: Vec<PathBuf>,
    pub errors: Vec<OpError>,
    /// Size of the inputs and of the outputs that were made, in bytes.
    pub before: u64,
    pub after: u64,
    pub cancelled: bool,
}

// ------------------------------------------------------------------ images

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Webp,
    Avif,
    Jpeg,
    Png,
}

impl ImageFormat {
    pub const ALL: [ImageFormat; 4] = [Self::Webp, Self::Avif, Self::Jpeg, Self::Png];

    pub fn label(self) -> &'static str {
        match self {
            Self::Webp => "WebP",
            Self::Avif => "AVIF",
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }

    pub fn lossy(self) -> bool {
        self != Self::Png
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageOptions {
    pub format: ImageFormat,
    /// 1–100, for lossy formats.
    pub quality: u8,
    /// Shrink so neither side is longer (never enlarges).
    pub max_side: Option<u32>,
}

/// Still-image extensions ImageMagick reads (animated GIFs are left alone).
pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "avif", "heic", "heif", "bmp", "tif", "tiff", "jxl"];

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn is_image(p: &Path) -> bool {
    IMAGE_EXTS.contains(&ext_of(p).as_str())
}

/// The files `inputs` stand for: images as they are, and every image inside
/// folders (recursively, skipping hidden ones).
pub fn collect(inputs: &[PathBuf], want: fn(&Path) -> bool) -> Vec<PathBuf> {
    fn walk(dir: &Path, want: fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.')).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                walk(&e.path(), want, out);
            } else if ft.is_file() && want(&e.path()) {
                out.push(e.path());
            }
        }
    }
    let mut out = Vec::new();
    for p in inputs {
        if p.is_dir() {
            walk(p, want, &mut out);
        } else if want(p) {
            out.push(p.clone());
        }
    }
    out
}

/// A free name next to `input` with extension `ext`.
pub fn output_for(input: &Path, ext: &str, taken: &dyn Fn(&str) -> bool) -> PathBuf {
    let dir = input.parent().unwrap_or(Path::new("."));
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    dir.join(names::unique_name(dir, &format!("{stem}.{ext}"), taken))
}

/// ImageMagick arguments (after the program name).
pub fn image_args(input: &Path, output: &Path, o: &ImageOptions) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec![input.into(), "-auto-orient".into()];
    if let Some(n) = o.max_side {
        a.extend(["-resize".into(), format!("{n}x{n}>").into()]);
    }
    // Camera data and profiles are most of a small file; drop them.
    a.push("-strip".into());
    if o.format.lossy() {
        a.extend(["-quality".into(), o.quality.clamp(1, 100).to_string().into()]);
    } else {
        a.extend(["-define".into(), "png:compression-level=9".into()]);
    }
    // The prefix fixes the format whatever the name says.
    let mut out = OsString::from(format!("{}:", o.format.ext().replace("jpg", "jpeg")));
    out.push(output);
    a.push(out);
    a
}

/// Convert `files` with ImageMagick (`program` is `magick` or `convert`),
/// a few at a time.
pub fn convert_images(program: &str, files: &[PathBuf], o: &ImageOptions, obs: &mut dyn Observer, cancel: &Cancel) -> Outcome {
    let total = files.len() as u64;
    let next = AtomicUsize::new(0);
    // Reserve output names up front so parallel workers never collide.
    let mut reserved: std::collections::HashSet<PathBuf> = Default::default();
    let outputs: Vec<PathBuf> = files
        .iter()
        .map(|f| {
            let dir = f.parent().unwrap_or(Path::new("."));
            let p = output_for(f, o.format.ext(), &|n| reserved.contains(&dir.join(n)));
            reserved.insert(p.clone());
            p
        })
        .collect();
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4);
    let (tx, rx) = mpsc::channel::<(usize, Result<(), String>)>();
    let mut out = Outcome::default();
    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (next, outputs) = (&next, &outputs);
            s.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= files.len() || cancel.is_cancelled() {
                    return;
                }
                let r = run(Command::new(program).args(image_args(&files[i], &outputs[i], o)), cancel);
                if r.is_err() {
                    let _ = std::fs::remove_file(&outputs[i]);
                }
                let _ = tx.send((i, r));
            });
        }
        drop(tx);
        for (done, (i, r)) in rx.into_iter().enumerate() {
            obs.progress(&Progress { files_done: done as u64 + 1, files_total: total, current: name(&files[i]), ..Default::default() });
            record(&mut out, &files[i], &outputs[i], r);
        }
    });
    out.cancelled = cancel.is_cancelled();
    out
}

fn name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn record(out: &mut Outcome, input: &Path, output: &Path, r: Result<(), String>) {
    match r {
        Ok(()) => {
            out.before += std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
            out.after += std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
            out.made.push(output.to_path_buf());
        }
        Err(e) if e == CANCELLED => {}
        Err(e) => out.errors.push(OpError::new(input, format!("“{}” couldn't be converted.", name(input)), e)),
    }
}

const CANCELLED: &str = "cancelled";

/// Run to the end (or kill it on cancel); the error is its last stderr line.
fn run(cmd: &mut Command, cancel: &Cancel) -> Result<(), String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let mut err = String::new();
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let status = loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.into());
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(Duration::from_millis(30)),
            Err(e) => return Err(e.to_string()),
        }
    };
    if let Ok(s) = reader.join() {
        err = s;
    }
    if status.success() {
        Ok(())
    } else {
        Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("The converter failed.").trim().to_string())
    }
}

// ------------------------------------------------------------------ videos

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoQuality {
    High,
    Balanced,
    Small,
}

impl VideoQuality {
    pub const ALL: [VideoQuality; 3] = [Self::High, Self::Balanced, Self::Small];

    pub fn label(self) -> &'static str {
        match self {
            Self::High => "High quality",
            Self::Balanced => "Balanced",
            Self::Small => "Smallest file",
        }
    }

    /// x264 constant rate factor and audio bitrate.
    fn crf_audio(self) -> (u8, &'static str) {
        match self {
            Self::High => (20, "192k"),
            Self::Balanced => (24, "128k"),
            Self::Small => (30, "96k"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoOptions {
    pub quality: VideoQuality,
    /// Scale down to this height (never up).
    pub max_height: Option<u32>,
}

pub const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp", "ogv"];

pub fn is_video(p: &Path) -> bool {
    VIDEO_EXTS.contains(&ext_of(p).as_str())
}

/// FFmpeg arguments: H.264 + AAC in MP4, which plays everywhere.
pub fn video_args(input: &Path, output: &Path, o: &VideoOptions) -> Vec<OsString> {
    let (crf, audio) = o.quality.crf_audio();
    let mut a: Vec<OsString> = ["-hide_banner", "-nostdin", "-y", "-i"].map(OsString::from).to_vec();
    a.push(input.into());
    if let Some(h) = o.max_height {
        // Even width (H.264 needs it), never upscale.
        a.extend(["-vf".into(), format!("scale=-2:'min({h},ih)'").into()]);
    }
    for s in ["-c:v", "libx264", "-preset", "medium", "-crf", &crf.to_string(), "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", audio, "-movflags", "+faststart", "-progress", "pipe:1", "-nostats"] {
        a.push(s.into());
    }
    a.push(output.into());
    a
}

/// Seconds from FFmpeg's "  Duration: 00:01:02.50, start: …" line.
pub fn parse_duration(line: &str) -> Option<f64> {
    let t = line.trim().strip_prefix("Duration:")?.trim();
    let t = t.split(',').next()?;
    let mut secs = 0.0;
    for part in t.split(':') {
        secs = secs * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(secs)
}

/// Compress `files` one after another (FFmpeg already uses every core).
pub fn compress_videos(files: &[PathBuf], o: &VideoOptions, obs: &mut dyn Observer, cancel: &Cancel) -> Outcome {
    ffmpeg_each(files, "mp4", &|i, out| video_args(i, out, o), obs, cancel)
}

/// Run FFmpeg on each file, writing `<name>.<ext>` beside it.
fn ffmpeg_each(files: &[PathBuf], ext: &str, args: &dyn Fn(&Path, &Path) -> Vec<OsString>, obs: &mut dyn Observer, cancel: &Cancel) -> Outcome {
    let sizes: Vec<u64> = files.iter().map(|f| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0)).collect();
    let total: u64 = sizes.iter().sum();
    let mut before_this = 0u64;
    let mut out = Outcome::default();
    for (i, f) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        let output = output_for(f, ext, &|_| false);
        let size = sizes[i];
        let mut report = |frac: f64| {
            obs.progress(&Progress {
                files_done: i as u64,
                files_total: files.len() as u64,
                bytes_done: before_this + (size as f64 * frac.clamp(0.0, 1.0)) as u64,
                bytes_total: total,
                current: name(f),
            })
        };
        report(0.0);
        let r = run_ffmpeg(&args(f, &output), cancel, &mut report);
        if r.is_err() {
            let _ = std::fs::remove_file(&output);
        }
        record(&mut out, f, &output, r);
        before_this += size;
    }
    out.cancelled = cancel.is_cancelled();
    out
}

// ------------------------------------------------------------------- audio

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    AacM4a,
    AlacM4a,
    Flac,
    Opus,
    Wav,
}

impl AudioFormat {
    pub const ALL: [AudioFormat; 6] = [Self::Mp3, Self::AacM4a, Self::AlacM4a, Self::Flac, Self::Opus, Self::Wav];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp3 => "MP3",
            Self::AacM4a => "M4A (AAC)",
            Self::AlacM4a => "M4A (Apple Lossless)",
            Self::Flac => "FLAC",
            Self::Opus => "Opus",
            Self::Wav => "WAV",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::AacM4a | Self::AlacM4a => "m4a",
            Self::Flac => "flac",
            Self::Opus => "opus",
            Self::Wav => "wav",
        }
    }

    /// Lossy formats take a bitrate.
    pub fn lossy(self) -> bool {
        matches!(self, Self::Mp3 | Self::AacM4a | Self::Opus)
    }

    fn codec(self) -> &'static str {
        match self {
            Self::Mp3 => "libmp3lame",
            Self::AacM4a => "aac",
            Self::AlacM4a => "alac",
            Self::Flac => "flac",
            Self::Opus => "libopus",
            Self::Wav => "pcm_s16le",
        }
    }

    /// Can hold cover art (copied from the original).
    fn cover(self) -> bool {
        matches!(self, Self::Mp3 | Self::AacM4a | Self::AlacM4a | Self::Flac)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioOptions {
    pub format: AudioFormat,
    /// kbit/s, for lossy formats.
    pub bitrate: u32,
}

pub const AUDIO_EXTS: &[&str] = &["mp3", "m4a", "aac", "flac", "alac", "ogg", "oga", "opus", "wav", "aif", "aiff", "wma", "ape", "wv", "mka"];

pub fn is_audio(p: &Path) -> bool {
    AUDIO_EXTS.contains(&ext_of(p).as_str())
}

/// FFmpeg arguments: the first audio stream, tags kept, cover art copied
/// where the format can hold it.
pub fn audio_args(input: &Path, output: &Path, o: &AudioOptions) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-hide_banner", "-nostdin", "-y", "-i"].map(OsString::from).to_vec();
    a.push(input.into());
    let mut push = |s: &[&str]| a.extend(s.iter().map(OsString::from));
    push(&["-map", "0:a:0", "-map_metadata", "0"]);
    if o.format.cover() && is_audio(input) {
        // Cover art is a still "video" stream in audio files; `?` = if any.
        push(&["-map", "0:v:0?", "-c:v", "copy", "-disposition:v:0", "attached_pic"]);
    } else {
        push(&["-vn"]);
    }
    push(&["-c:a", o.format.codec()]);
    if o.format.lossy() {
        a.extend(["-b:a".into(), format!("{}k", o.bitrate.clamp(32, 512)).into()]);
    }
    a.extend(["-progress", "pipe:1", "-nostats"].map(OsString::from));
    a.push(output.into());
    a
}

/// Convert audio files one after another.
pub fn convert_audio(files: &[PathBuf], o: &AudioOptions, obs: &mut dyn Observer, cancel: &Cancel) -> Outcome {
    ffmpeg_each(files, o.format.ext(), &|i, out| audio_args(i, out, o), obs, cancel)
}

/// Run FFmpeg, reporting the fraction done from its `-progress` output.
fn run_ffmpeg(args: &[OsString], cancel: &Cancel, report: &mut dyn FnMut(f64)) -> Result<(), String> {
    let mut child = Command::new("ffmpeg").args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel::<Msg>();
    enum Msg {
        Duration(f64),
        Time(f64),
        Err(String),
    }
    let stderr = child.stderr.take().expect("piped");
    let t = tx.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let msg = parse_duration(&line).map(Msg::Duration).unwrap_or(Msg::Err(line));
            if t.send(msg).is_err() {
                return;
            }
        }
    });
    let stdout = child.stdout.take().expect("piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            // `out_time_us` (older FFmpeg: the misnamed `out_time_ms`, also µs).
            if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")).and_then(|v| v.parse::<f64>().ok())
                && tx.send(Msg::Time(v / 1e6)).is_err()
            {
                return;
            }
        }
    });
    let mut duration = 0.0;
    let mut last_err = String::new();
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.into());
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Msg::Duration(d)) if duration == 0.0 => duration = d,
            Ok(Msg::Time(t)) if duration > 0.0 => report(t / duration),
            Ok(Msg::Err(l)) if !l.trim().is_empty() => last_err = l,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Ok(Some(_)) = child.try_wait() {
            // Drain what's left, then stop.
            while let Ok(m) = rx.recv_timeout(Duration::from_millis(50)) {
                if let Msg::Err(l) = m
                    && !l.trim().is_empty()
                {
                    last_err = l;
                }
            }
            break;
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(if last_err.is_empty() { "FFmpeg failed.".into() } else { last_err.trim().to_string() }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_arguments() {
        let o = ImageOptions { format: ImageFormat::Jpeg, quality: 70, max_side: Some(1920) };
        let a = image_args(Path::new("/p/a b.png"), Path::new("/p/a b.jpg"), &o);
        let a: Vec<String> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(a, ["/p/a b.png", "-auto-orient", "-resize", "1920x1920>", "-strip", "-quality", "70", "jpeg:/p/a b.jpg"]);
        let o = ImageOptions { format: ImageFormat::Png, quality: 70, max_side: None };
        let a = image_args(Path::new("/x.jpg"), Path::new("/x.png"), &o);
        assert!(a.iter().any(|s| s == "png:compression-level=9") && !a.iter().any(|s| s == "-quality"));
    }

    #[test]
    fn outputs_never_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let photo = d.path().join("photo.jpg");
        std::fs::write(&photo, b"x").unwrap();
        assert_eq!(output_for(&photo, "webp", &|_| false), d.path().join("photo.webp"));
        // Same format: never the original.
        assert_eq!(output_for(&photo, "jpg", &|_| false), d.path().join("photo (2).jpg"));
        assert_eq!(output_for(&photo, "webp", &|n| n == "photo.webp"), d.path().join("photo (2).webp"));
    }

    #[test]
    fn collects_images_from_folders() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sub/.hidden")).unwrap();
        for f in ["a.JPG", "b.txt", "sub/c.png", "sub/.hidden/d.png", ".e.png"] {
            std::fs::write(d.path().join(f), b"x").unwrap();
        }
        let got = collect(&[d.path().to_path_buf()], is_image);
        assert_eq!(got, [d.path().join("a.JPG"), d.path().join("sub/c.png")]);
    }

    #[test]
    fn video_arguments_and_duration() {
        let o = VideoOptions { quality: VideoQuality::Balanced, max_height: Some(720) };
        let a: Vec<String> = video_args(Path::new("/v/in.mov"), Path::new("/v/in.mp4"), &o).iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(&a[..5], ["-hide_banner", "-nostdin", "-y", "-i", "/v/in.mov"]);
        assert!(a.windows(2).any(|w| w == ["-vf", "scale=-2:'min(720,ih)'"]));
        assert!(a.windows(2).any(|w| w == ["-crf", "24"]));
        assert_eq!(a.last().unwrap(), "/v/in.mp4");
        assert_eq!(parse_duration("  Duration: 00:01:02.50, start: 0.000000, bitrate: 1 kb/s"), Some(62.5));
        assert_eq!(parse_duration("  Duration: N/A, bitrate: N/A"), None);
        assert_eq!(parse_duration("Stream #0:0"), None);
    }

    #[test]
    fn audio_arguments() {
        let s = |v: Vec<OsString>| v.iter().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let o = AudioOptions { format: AudioFormat::Mp3, bitrate: 192 };
        let a = s(audio_args(Path::new("/m/a.flac"), Path::new("/m/a.mp3"), &o));
        assert!(a.windows(2).any(|w| w == ["-c:a", "libmp3lame"]) && a.windows(2).any(|w| w == ["-b:a", "192k"]));
        assert!(a.windows(2).any(|w| w == ["-map", "0:v:0?"]), "cover art kept");
        assert_eq!(a.last().unwrap(), "/m/a.mp3");
        // Lossless: no bitrate. Opus: no cover. A video's picture is never copied.
        let a = s(audio_args(Path::new("/m/a.m4a"), Path::new("/m/a.flac"), &AudioOptions { format: AudioFormat::Flac, bitrate: 192 }));
        assert!(!a.iter().any(|x| x == "-b:a"));
        let a = s(audio_args(Path::new("/m/a.flac"), Path::new("/m/a.opus"), &AudioOptions { format: AudioFormat::Opus, bitrate: 128 }));
        assert!(a.iter().any(|x| x == "-vn"));
        let a = s(audio_args(Path::new("/m/clip.mp4"), Path::new("/m/clip.m4a"), &AudioOptions { format: AudioFormat::AacM4a, bitrate: 256 }));
        assert!(a.iter().any(|x| x == "-vn"));
        assert!(is_audio(Path::new("x.FLAC")) && !is_audio(Path::new("x.mp4")));
    }

    /// Real conversion when ImageMagick is installed.
    #[test]
    fn converts_with_imagemagick() {
        let Some(prog) = ["magick", "convert"].into_iter().find(|p| crate::terminal::on_path(p)) else { return };
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("red.png");
        image::RgbImage::from_pixel(64, 48, image::Rgb([200, 30, 30])).save(&src).unwrap();
        let o = ImageOptions { format: ImageFormat::Webp, quality: 70, max_side: Some(32) };
        let out = convert_images(prog, &[src.clone(), src.clone()], &o, &mut crate::ops::Fixed(crate::ops::Resolution::Skip), &Cancel::new());
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.made.len(), 2);
        assert!(out.made.contains(&d.path().join("red.webp")) && out.made.contains(&d.path().join("red (2).webp")));
        let img = image::open(d.path().join("red.webp")).unwrap();
        assert_eq!((img.width(), img.height()), (32, 24));
    }
}
