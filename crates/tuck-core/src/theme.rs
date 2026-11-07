// SPDX-License-Identifier: GPL-3.0-or-later
//! Themes and accent colours, with readability guaranteed.
//!
//! Every theme and accent goes through [`resolve`], which nudges colours (in
//! OKLCH, keeping their hue) until all text meets WCAG AA (4.5:1) against
//! every surface it can appear on, including secondary text, accent-coloured
//! text and text on accent backgrounds. High Contrast aims for 7:1.
//! Translucent surfaces are checked as if composited over both a white and
//! a black wallpaper, so they stay readable over anything.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

// ------------------------------------------------------------------ colour

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Rgb {
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.trim().trim_start_matches('#');
        let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
        let (r, g, b) = match h.len() {
            6 => (v(0, 2)?, v(2, 2)?, v(4, 2)?),
            3 => {
                let x = |i: usize| v(i, 1).map(|c| c * 17);
                (x(0)?, x(1)?, x(2)?)
            }
            _ => return None,
        };
        Some(Rgb { r: r as f64 / 255.0, g: g as f64 / 255.0, b: b as f64 / 255.0 })
    }

    pub fn hex(self) -> String {
        let c = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", c(self.r), c(self.g), c(self.b))
    }

    fn lin(c: f64) -> f64 {
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    fn unlin(c: f64) -> f64 {
        if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f64 {
        0.2126 * Self::lin(self.r) + 0.7152 * Self::lin(self.g) + 0.0722 * Self::lin(self.b)
    }

    pub fn mix(self, other: Rgb, t: f64) -> Rgb {
        Rgb { r: self.r + (other.r - self.r) * t, g: self.g + (other.g - self.g) * t, b: self.b + (other.b - self.b) * t }
    }

    /// This colour at `alpha`, composited over `under`.
    pub fn over(self, alpha: f64, under: Rgb) -> Rgb {
        under.mix(self, alpha)
    }

    fn to_oklch(self) -> (f64, f64, f64) {
        let (r, g, b) = (Self::lin(self.r), Self::lin(self.g), Self::lin(self.b));
        let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
        let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
        let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
        let ll = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
        let a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
        let bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
        (ll, (a * a + bb * bb).sqrt(), bb.atan2(a))
    }

    fn from_oklch(l: f64, c: f64, h: f64) -> Option<Rgb> {
        let (a, b) = (c * h.cos(), c * h.sin());
        let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
        let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
        let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
        let r = 4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_;
        let g = -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_;
        let bl = -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_;
        let ok = |x: f64| (-1e-4..=1.0001).contains(&x);
        (ok(r) && ok(g) && ok(bl)).then(|| Rgb { r: Self::unlin(r.clamp(0.0, 1.0)), g: Self::unlin(g.clamp(0.0, 1.0)), b: Self::unlin(bl.clamp(0.0, 1.0)) })
    }

    /// Same hue at OKLCH lightness `l`, reducing chroma until it fits sRGB.
    fn with_lightness(self, l: f64) -> Rgb {
        let (_, mut c, h) = self.to_oklch();
        loop {
            if let Some(rgb) = Rgb::from_oklch(l, c, h) {
                return rgb;
            }
            c *= 0.9;
            if c < 1e-4 {
                return Rgb::from_oklch(l, 0.0, h).unwrap_or(self);
            }
        }
    }
}

pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (x, y) = (a.luminance(), b.luminance());
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The smallest lightness change to `fg` (keeping hue) that reaches
/// `target` contrast against every colour in `bgs`.
pub fn ensure_contrast(fg: Rgb, bgs: &[Rgb], target: f64) -> Rgb {
    let worst = |c: Rgb| bgs.iter().map(|b| contrast(c, *b)).fold(f64::INFINITY, f64::min);
    if worst(fg) >= target {
        return fg;
    }
    let (l0, _, _) = fg.to_oklch();
    // Try both directions; keep whichever passes with the smaller change.
    let mut best: Option<(f64, Rgb)> = None;
    for dir in [-1.0, 1.0] {
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64); // fraction of the way to the extreme
        let end = if dir < 0.0 { 0.0 } else { 1.0 };
        let at = |t: f64| fg.with_lightness(l0 + (end - l0) * t);
        if worst(at(1.0)) < target {
            continue;
        }
        for _ in 0..30 {
            let mid = (lo + hi) / 2.0;
            if worst(at(mid)) >= target { hi = mid } else { lo = mid }
        }
        let c = at(hi);
        let change = ((end - l0) * hi).abs();
        if best.as_ref().is_none_or(|(d, _)| change < *d) {
            best = Some((change, c));
        }
    }
    best.map(|(_, c)| c).unwrap_or_else(|| {
        // Neither direction reaches the target: the best of black and white.
        let (w, k) = (Rgb { r: 1.0, g: 1.0, b: 1.0 }, Rgb { r: 0.0, g: 0.0, b: 0.0 });
        if worst(w) >= worst(k) { w } else { k }
    })
}

// ------------------------------------------------------------------ themes

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Colors {
    pub window_bg: String,
    pub window_fg: String,
    pub view_bg: Option<String>,
    pub view_fg: Option<String>,
    pub sidebar_bg: Option<String>,
    pub sidebar_fg: Option<String>,
    pub headerbar_bg: Option<String>,
    pub headerbar_fg: Option<String>,
    pub card_bg: Option<String>,
    pub popover_bg: Option<String>,
    /// Secondary text.
    pub dim_fg: Option<String>,
    pub accent: Option<String>,
    pub destructive: Option<String>,
    pub success: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Theme {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub dark: bool,
    #[serde(default)]
    pub translucent: bool,
    #[serde(default)]
    pub high_contrast: bool,
    pub colors: Colors,
}

const BUILTIN: &[(&str, &str)] = &[
    ("light", r##"name = "Light"
dark = false
[colors]
window_bg = "#f6f6f8"
window_fg = "#1d1d22"
view_bg = "#ffffff"
sidebar_bg = "#eeeef2"
headerbar_bg = "#f6f6f8"
card_bg = "#ffffff"
dim_fg = "#5e5e68"
accent = "#3584e4"
destructive = "#e01b24"
success = "#26a269"
warning = "#c64600"
"##),
    ("dark", r##"name = "Dark"
dark = true
[colors]
window_bg = "#1e1e22"
window_fg = "#f1f1f4"
view_bg = "#18181b"
sidebar_bg = "#25252a"
headerbar_bg = "#25252a"
card_bg = "#2c2c32"
popover_bg = "#2c2c32"
dim_fg = "#a8a8b3"
accent = "#78aeed"
destructive = "#ff7b63"
success = "#8ff0a4"
warning = "#f8e45c"
"##),
    ("amoled", r##"name = "AMOLED Black"
dark = true
[colors]
window_bg = "#000000"
window_fg = "#f2f2f5"
view_bg = "#000000"
sidebar_bg = "#000000"
headerbar_bg = "#000000"
card_bg = "#111114"
popover_bg = "#111114"
dim_fg = "#9a9aa5"
accent = "#78aeed"
destructive = "#ff7b63"
success = "#8ff0a4"
warning = "#f8e45c"
"##),
    ("dracula", r##"name = "Dracula"
dark = true
[colors]
window_bg = "#282a36"
window_fg = "#f8f8f2"
view_bg = "#21222c"
sidebar_bg = "#1e1f29"
headerbar_bg = "#191a21"
card_bg = "#343746"
popover_bg = "#343746"
dim_fg = "#a0a6c6"
accent = "#bd93f9"
destructive = "#ff5555"
success = "#50fa7b"
warning = "#f1fa8c"
"##),
    ("material", r##"name = "Material"
dark = true
[colors]
window_bg = "#141218"
window_fg = "#e6e0e9"
view_bg = "#141218"
sidebar_bg = "#1d1b20"
headerbar_bg = "#211f26"
card_bg = "#2b2930"
popover_bg = "#2b2930"
dim_fg = "#cac4d0"
accent = "#d0bcff"
destructive = "#f2b8b5"
success = "#a6d8a0"
warning = "#ffd479"
"##),
    ("translucent", r##"name = "Translucent"
dark = true
translucent = true
[colors]
window_bg = "#1b1b21"
window_fg = "#f4f4f7"
view_bg = "#16161b"
sidebar_bg = "#202027"
headerbar_bg = "#202027"
card_bg = "#2a2a31"
popover_bg = "#2a2a31"
dim_fg = "#b3b3be"
accent = "#7cb0ff"
"##),
    ("nord", r##"name = "Nord"
dark = true
[colors]
window_bg = "#2e3440"
window_fg = "#eceff4"
view_bg = "#2b303b"
sidebar_bg = "#3b4252"
headerbar_bg = "#3b4252"
card_bg = "#434c5e"
popover_bg = "#3b4252"
dim_fg = "#c2cad8"
accent = "#88c0d0"
destructive = "#bf616a"
success = "#a3be8c"
warning = "#ebcb8b"
"##),
    ("catppuccin", r##"name = "Catppuccin"
dark = true
[colors]
window_bg = "#1e1e2e"
window_fg = "#cdd6f4"
view_bg = "#181825"
sidebar_bg = "#181825"
headerbar_bg = "#11111b"
card_bg = "#313244"
popover_bg = "#313244"
dim_fg = "#a6adc8"
accent = "#cba6f7"
destructive = "#f38ba8"
success = "#a6e3a1"
warning = "#f9e2af"
"##),
    ("tokyo-night", r##"name = "Tokyo Night"
dark = true
[colors]
window_bg = "#1a1b26"
window_fg = "#c0caf5"
view_bg = "#16161e"
sidebar_bg = "#16161e"
headerbar_bg = "#1f2335"
card_bg = "#292e42"
popover_bg = "#292e42"
dim_fg = "#9aa5ce"
accent = "#7aa2f7"
destructive = "#f7768e"
success = "#9ece6a"
warning = "#e0af68"
"##),
    ("high-contrast", r##"name = "High Contrast"
dark = false
high_contrast = true
[colors]
window_bg = "#ffffff"
window_fg = "#000000"
view_bg = "#ffffff"
sidebar_bg = "#ffffff"
headerbar_bg = "#ffffff"
card_bg = "#ffffff"
dim_fg = "#1a1a1a"
accent = "#0039a6"
destructive = "#a00000"
success = "#005c00"
warning = "#6b3e00"
"##),
];

/// Theme choices shown in Preferences. "system" follows the desktop.
pub const CHOICES: &[(&str, &str)] = &[
    ("system", "Follow System"),
    ("light", "Light"),
    ("dark", "Dark"),
    ("amoled", "AMOLED Black"),
    ("dracula", "Dracula"),
    ("material", "Material"),
    ("translucent", "Translucent"),
    ("nord", "Nord"),
    ("catppuccin", "Catppuccin"),
    ("tokyo-night", "Tokyo Night"),
    ("high-contrast", "High Contrast"),
];

pub fn builtin(id: &str) -> Option<Theme> {
    let (_, text) = BUILTIN.iter().find(|(i, _)| *i == id)?;
    let mut t: Theme = toml::from_str(text).expect("built-in themes parse");
    t.id = id.to_string();
    Some(t)
}

pub fn builtins() -> Vec<Theme> {
    BUILTIN.iter().filter_map(|(id, _)| builtin(id)).collect()
}

/// Load a user theme file. Errors are explained in plain words.
pub fn load_user_theme(path: &Path) -> Result<Theme, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let mut t: Theme = toml::from_str(&text).map_err(|e| format!("{} isn't a valid theme: {}", path.display(), e.message()))?;
    for (key, v) in t.colors.all() {
        if let Some(v) = v
            && Rgb::parse(v).is_none() {
                return Err(format!("{}: “{v}” for {key} isn't a colour like #1e1e2e.", path.display()));
            }
    }
    t.id = format!("user:{}", path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    Ok(t)
}

/// User themes from `dir` (`*.toml`), with any that failed to load.
pub fn user_themes(dir: &Path) -> (Vec<Theme>, Vec<String>) {
    let mut ok = Vec::new();
    let mut errors = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return (ok, errors) };
    let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")).collect();
    files.sort();
    for f in files {
        match load_user_theme(&f) {
            Ok(t) => ok.push(t),
            Err(e) => errors.push(e),
        }
    }
    (ok, errors)
}

impl Colors {
    fn all(&self) -> Vec<(&'static str, Option<&String>)> {
        vec![
            ("window_bg", Some(&self.window_bg)),
            ("window_fg", Some(&self.window_fg)),
            ("view_bg", self.view_bg.as_ref()),
            ("view_fg", self.view_fg.as_ref()),
            ("sidebar_bg", self.sidebar_bg.as_ref()),
            ("sidebar_fg", self.sidebar_fg.as_ref()),
            ("headerbar_bg", self.headerbar_bg.as_ref()),
            ("headerbar_fg", self.headerbar_fg.as_ref()),
            ("card_bg", self.card_bg.as_ref()),
            ("popover_bg", self.popover_bg.as_ref()),
            ("dim_fg", self.dim_fg.as_ref()),
            ("accent", self.accent.as_ref()),
            ("destructive", self.destructive.as_ref()),
            ("success", self.success.as_ref()),
            ("warning", self.warning.as_ref()),
        ]
    }
}

// ----------------------------------------------------------------- accents

pub const ACCENTS: &[(&str, &str, &str)] = &[
    ("theme", "Theme Default", ""),
    ("blue", "Blue", "#3584e4"),
    ("teal", "Teal", "#2190a4"),
    ("green", "Green", "#3a944a"),
    ("yellow", "Yellow", "#c88800"),
    ("orange", "Orange", "#ed5b00"),
    ("red", "Red", "#e62d42"),
    ("pink", "Pink", "#d56199"),
    ("purple", "Purple", "#9141ac"),
    ("slate", "Slate", "#6f8396"),
];

/// Accent setting: a preset id, "theme", or a custom "#rrggbb".
pub fn accent_hex<'a>(choice: &'a str, theme: &'a Theme) -> Option<&'a str> {
    if choice.starts_with('#') {
        return Some(choice);
    }
    match ACCENTS.iter().find(|(id, _, _)| *id == choice) {
        Some((_, _, "")) | None => theme.colors.accent.as_deref(),
        Some((_, _, hex)) => Some(hex),
    }
}

// ----------------------------------------------------------------- palette

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub high_contrast: bool,
    /// Effective opacity of each surface over the wallpaper (1.0 = solid).
    /// Inner surfaces sit on the window background, so their own CSS alpha
    /// is smaller: see [`Palette::layer_alpha`].
    pub window_alpha: f64,
    pub view_alpha: f64,
    pub sidebar_alpha: f64,
    pub window_bg: Rgb,
    pub window_fg: Rgb,
    pub view_bg: Rgb,
    pub view_fg: Rgb,
    pub sidebar_bg: Rgb,
    pub sidebar_fg: Rgb,
    pub headerbar_bg: Rgb,
    pub headerbar_fg: Rgb,
    pub card_bg: Rgb,
    pub popover_bg: Rgb,
    pub dim_fg: Rgb,
    pub sidebar_dim_fg: Rgb,
    pub border: Rgb,
    pub accent_bg: Rgb,
    pub accent_fg: Rgb,
    /// Accent used as text or icon colour on normal surfaces.
    pub accent_color: Rgb,
    /// Background of selected items, and the text on it.
    pub selection_bg: Rgb,
    pub destructive_bg: Rgb,
    pub destructive_fg: Rgb,
    pub destructive_color: Rgb,
    pub success_color: Rgb,
    pub warning_color: Rgb,
}

const WHITE: Rgb = Rgb { r: 1.0, g: 1.0, b: 1.0 };
const INK: Rgb = Rgb { r: 0.08, g: 0.08, b: 0.1 };

/// Text colour (white or near-black) for a solid background, adjusting the
/// background if neither reaches `target`.
fn on_color(bg: Rgb, target: f64) -> (Rgb, Rgb) {
    let fg = if contrast(WHITE, bg) >= contrast(INK, bg) { WHITE } else { INK };
    if contrast(fg, bg) >= target {
        return (bg, fg);
    }
    (ensure_contrast(bg, &[fg], target), fg)
}

/// Resolve a theme with an accent into concrete, readable colours.
/// `allow_translucency` is false when no compositor is running.
pub fn resolve(theme: &Theme, accent_choice: &str, allow_translucency: bool) -> Palette {
    let c = &theme.colors;
    let p = |s: &Option<String>, fallback: Rgb| s.as_deref().and_then(Rgb::parse).unwrap_or(fallback);
    let window_bg = Rgb::parse(&c.window_bg).unwrap_or(WHITE);
    let window_fg0 = Rgb::parse(&c.window_fg).unwrap_or(INK);
    let target = if theme.high_contrast { 7.0 } else { 4.5 };
    let view_bg = p(&c.view_bg, window_bg);
    let sidebar_bg = p(&c.sidebar_bg, window_bg.mix(window_fg0, 0.04));
    let headerbar_bg = p(&c.headerbar_bg, window_bg);
    let card_bg = p(&c.card_bg, view_bg.mix(window_fg0, 0.04));
    let popover_bg = p(&c.popover_bg, card_bg);

    // Translucent: surfaces are partly see-through. Text must work over a
    // white or a black wallpaper, so raise opacity until it does.
    let translucent = theme.translucent && allow_translucency;
    let solid = |bg: Rgb, alpha: f64| [bg.over(alpha, WHITE), bg.over(alpha, Rgb { r: 0.0, g: 0.0, b: 0.0 })];
    let fit_alpha = |bg: Rgb, fg: Rgb, start: f64| -> f64 {
        if !translucent {
            return 1.0;
        }
        let mut a = start;
        while a < 1.0 && solid(bg, a).iter().any(|s| contrast(fg, *s) < target + 0.5) {
            a += 0.02;
        }
        a.min(1.0)
    };
    let window_fg = ensure_contrast(window_fg0, &[window_bg, headerbar_bg, card_bg, popover_bg], target);
    let view_fg = ensure_contrast(p(&c.view_fg, window_fg), &[view_bg, card_bg], target);
    let sidebar_fg = ensure_contrast(p(&c.sidebar_fg, window_fg), &[sidebar_bg], target);
    let headerbar_fg = ensure_contrast(p(&c.headerbar_fg, window_fg), &[headerbar_bg], target);
    // Effective opacities: the window first, then surfaces stacked on it
    // can only be as transparent as the window under them.
    let window_alpha = fit_alpha(window_bg, window_fg, 0.78);
    let view_alpha = fit_alpha(view_bg, view_fg, window_alpha.max(0.84));
    let sidebar_alpha = fit_alpha(sidebar_bg, sidebar_fg, window_alpha);
    let bgs_main: Vec<Rgb> = {
        let mut v = vec![window_bg, view_bg, card_bg, popover_bg, headerbar_bg];
        if translucent {
            v.extend(solid(window_bg, window_alpha));
            v.extend(solid(view_bg, view_alpha));
        }
        v
    };
    let mut sidebar_bgs = vec![sidebar_bg];
    if translucent {
        sidebar_bgs.extend(solid(sidebar_bg, sidebar_alpha));
    }
    let dim0 = p(&c.dim_fg, window_fg.mix(window_bg, 0.32));
    let dim_fg = ensure_contrast(dim0, &bgs_main, target);
    let sidebar_dim_fg = ensure_contrast(dim0, &sidebar_bgs, target);
    let border = window_fg.mix(window_bg, if theme.high_contrast { 0.0 } else { 0.84 });

    let accent = accent_hex(accent_choice, theme).and_then(Rgb::parse).unwrap_or(Rgb { r: 0.21, g: 0.52, b: 0.89 });
    let (accent_bg, accent_fg) = on_color(accent, target);
    let mut text_bgs = bgs_main.clone();
    text_bgs.extend(sidebar_bgs.iter().copied());
    let accent_color = ensure_contrast(accent, &text_bgs, target);
    // Selected items: a light tint of the accent; the normal text must stay readable.
    let selection_bg = view_bg.mix(accent_bg, if theme.high_contrast { 0.0 } else { 0.22 });
    let destructive = p(&c.destructive, Rgb { r: 0.88, g: 0.11, b: 0.14 });
    let (destructive_bg, destructive_fg) = on_color(destructive, target);
    let destructive_color = ensure_contrast(destructive, &text_bgs, target);
    let success_color = ensure_contrast(p(&c.success, Rgb { r: 0.15, g: 0.63, b: 0.41 }), &text_bgs, target);
    let warning_color = ensure_contrast(p(&c.warning, Rgb { r: 0.78, g: 0.27, b: 0.0 }), &text_bgs, target);

    Palette {
        dark: theme.dark,
        high_contrast: theme.high_contrast,
        window_alpha,
        view_alpha,
        sidebar_alpha,
        window_bg,
        window_fg,
        view_bg,
        view_fg: ensure_contrast(view_fg, &[selection_bg], target),
        sidebar_bg,
        sidebar_fg,
        headerbar_bg,
        headerbar_fg,
        card_bg,
        popover_bg,
        dim_fg,
        sidebar_dim_fg,
        border,
        accent_bg,
        accent_fg,
        accent_color,
        selection_bg,
        destructive_bg,
        destructive_fg,
        destructive_color,
        success_color,
        warning_color,
    }
}

impl Palette {
    /// CSS alpha for a surface drawn on top of the window background so that
    /// together they reach `effective` opacity.
    pub fn layer_alpha(&self, effective: f64) -> f64 {
        if self.window_alpha >= 0.999 || effective >= 0.999 {
            return effective;
        }
        (1.0 - (1.0 - effective) / (1.0 - self.window_alpha)).clamp(0.0, 1.0)
    }
}

fn rgba(c: Rgb, a: f64) -> String {
    if a >= 0.999 {
        c.hex()
    } else {
        format!("rgba({}, {}, {}, {:.2})", (c.r * 255.0).round(), (c.g * 255.0).round(), (c.b * 255.0).round(), a)
    }
}

impl Palette {
    /// CSS that overrides libadwaita's colour variables, plus Tuck's own.
    pub fn css(&self) -> String {
        let mut v: BTreeMap<&str, String> = BTreeMap::new();
        v.insert("window-bg-color", rgba(self.window_bg, self.window_alpha));
        v.insert("window-fg-color", self.window_fg.hex());
        v.insert("view-bg-color", rgba(self.view_bg, self.layer_alpha(self.view_alpha)));
        v.insert("view-fg-color", self.view_fg.hex());
        v.insert("headerbar-bg-color", rgba(self.headerbar_bg, self.window_alpha));
        v.insert("headerbar-fg-color", self.headerbar_fg.hex());
        v.insert("headerbar-backdrop-color", rgba(self.headerbar_bg, self.window_alpha));
        v.insert("headerbar-border-color", self.headerbar_fg.hex());
        v.insert("headerbar-shade-color", rgba(self.border, 0.6));
        v.insert("sidebar-bg-color", rgba(self.sidebar_bg, self.layer_alpha(self.sidebar_alpha)));
        v.insert("sidebar-fg-color", self.sidebar_fg.hex());
        v.insert("sidebar-backdrop-color", rgba(self.sidebar_bg, self.layer_alpha(self.sidebar_alpha)));
        v.insert("sidebar-shade-color", rgba(self.border, 0.6));
        v.insert("secondary-sidebar-bg-color", rgba(self.sidebar_bg, self.layer_alpha(self.sidebar_alpha)));
        v.insert("secondary-sidebar-fg-color", self.sidebar_fg.hex());
        v.insert("secondary-sidebar-backdrop-color", rgba(self.sidebar_bg, self.layer_alpha(self.sidebar_alpha)));
        v.insert("card-bg-color", self.card_bg.hex());
        v.insert("card-fg-color", self.view_fg.hex());
        v.insert("dialog-bg-color", self.card_bg.hex());
        v.insert("dialog-fg-color", self.window_fg.hex());
        v.insert("popover-bg-color", self.popover_bg.hex());
        v.insert("popover-fg-color", self.window_fg.hex());
        v.insert("thumbnail-bg-color", self.card_bg.hex());
        v.insert("thumbnail-fg-color", self.view_fg.hex());
        v.insert("accent-bg-color", self.accent_bg.hex());
        v.insert("accent-fg-color", self.accent_fg.hex());
        v.insert("accent-color", self.accent_color.hex());
        v.insert("destructive-bg-color", self.destructive_bg.hex());
        v.insert("destructive-fg-color", self.destructive_fg.hex());
        v.insert("destructive-color", self.destructive_color.hex());
        v.insert("error-bg-color", self.destructive_bg.hex());
        v.insert("error-fg-color", self.destructive_fg.hex());
        v.insert("error-color", self.destructive_color.hex());
        v.insert("success-color", self.success_color.hex());
        v.insert("warning-color", self.warning_color.hex());
        v.insert("border-color", rgba(self.border, if self.high_contrast { 1.0 } else { 0.9 }));
        v.insert("tuck-dim-fg", self.dim_fg.hex());
        v.insert("tuck-sidebar-dim-fg", self.sidebar_dim_fg.hex());
        v.insert("tuck-selection-bg", self.selection_bg.hex());
        let mut css = String::from(":root {\n");
        for (k, val) in &v {
            css.push_str(&format!("  --{k}: {val};\n"));
        }
        css.push_str("}\n");
        if self.window_alpha < 1.0 {
            // The window itself carries the see-through colour; libadwaita's
            // header bars are transparent and show it.
            css.push_str(&format!("window.tuck-main {{ background-color: {}; }}\n", rgba(self.window_bg, self.window_alpha)));
        }
        if self.high_contrast {
            css.push_str(concat!(
                "button, entry, .card, row:focus-visible, gridview > child:focus-visible { outline-width: 3px; }\n",
                "button:not(.flat), entry { border: 2px solid var(--window-fg-color); }\n",
                "gridview.file-grid > child:selected, columnview.file-details > listview > row:selected { outline: 3px solid var(--accent-color); outline-offset: -3px; }\n",
            ));
        }
        css
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_math() {
        let w = Rgb::parse("#fff").unwrap();
        let k = Rgb::parse("#000000").unwrap();
        assert!((contrast(w, k) - 21.0).abs() < 1e-9);
        assert_eq!(Rgb::parse("#3584e4").unwrap().hex(), "#3584e4");
        assert!(Rgb::parse("#12").is_none());
        // OKLCH round trip keeps the colour.
        let c = Rgb::parse("#9141ac").unwrap();
        let (l, _, _) = c.to_oklch();
        assert_eq!(c.with_lightness(l).hex(), "#9141ac");
    }

    #[test]
    fn ensure_contrast_changes_little_and_keeps_hue() {
        let bg = Rgb::parse("#ffffff").unwrap();
        let yellow = Rgb::parse("#f8e45c").unwrap();
        let fixed = ensure_contrast(yellow, &[bg], 4.5);
        assert!(contrast(fixed, bg) >= 4.5);
        let (_, _, h0) = yellow.to_oklch();
        let (_, _, h1) = fixed.to_oklch();
        assert!((h0 - h1).abs() < 0.15, "hue preserved");
        let ok = Rgb::parse("#1d1d22").unwrap();
        assert_eq!(ensure_contrast(ok, &[bg], 4.5), ok, "already fine: unchanged");
    }

    fn check(p: &Palette, theme: &str, accent: &str) {
        let t = if p.high_contrast { 7.0 } else { 4.5 };
        let ctx = format!("{theme}/{accent}");
        let solids = |bg: Rgb, a: f64| -> Vec<Rgb> {
            if a < 1.0 { vec![bg.over(a, WHITE), bg.over(a, Rgb { r: 0.0, g: 0.0, b: 0.0 })] } else { vec![bg] }
        };
        let main: Vec<Rgb> = [solids(p.window_bg, p.window_alpha), solids(p.view_bg, p.view_alpha), vec![p.card_bg, p.popover_bg, p.headerbar_bg]].concat();
        let side = solids(p.sidebar_bg, p.sidebar_alpha);
        for bg in &main {
            assert!(contrast(p.dim_fg, *bg) >= t - 0.01, "{ctx}: dim text {:.2}", contrast(p.dim_fg, *bg));
            assert!(contrast(p.accent_color, *bg) >= t - 0.01, "{ctx}: accent text {:.2}", contrast(p.accent_color, *bg));
            assert!(contrast(p.destructive_color, *bg) >= t - 0.01, "{ctx}: destructive text");
        }
        for bg in solids(p.window_bg, p.window_alpha) {
            assert!(contrast(p.window_fg, bg) >= t - 0.01, "{ctx}: window text {:.2}", contrast(p.window_fg, bg));
        }
        for bg in solids(p.view_bg, p.view_alpha) {
            assert!(contrast(p.view_fg, bg) >= t - 0.01, "{ctx}: view text");
        }
        for bg in &side {
            assert!(contrast(p.sidebar_fg, *bg) >= t - 0.01, "{ctx}: sidebar text");
            assert!(contrast(p.sidebar_dim_fg, *bg) >= t - 0.01, "{ctx}: sidebar headings");
            assert!(contrast(p.accent_color, *bg) >= t - 0.01, "{ctx}: accent on sidebar");
        }
        assert!(contrast(p.accent_fg, p.accent_bg) >= t - 0.01, "{ctx}: text on accent {:.2}", contrast(p.accent_fg, p.accent_bg));
        assert!(contrast(p.destructive_fg, p.destructive_bg) >= t - 0.01, "{ctx}: text on destructive");
        assert!(contrast(p.view_fg, p.selection_bg) >= t - 0.01, "{ctx}: text on selection");
        assert!(contrast(p.headerbar_fg, p.headerbar_bg) >= t - 0.01, "{ctx}: header text");
    }

    #[test]
    fn every_theme_and_accent_passes_wcag_aa() {
        let mut accents: Vec<String> = ACCENTS.iter().map(|(id, _, _)| id.to_string()).collect();
        // Custom accents, including awkward ones.
        accents.extend(["#ffff00", "#ffffff", "#000000", "#777777", "#00ff88", "#1a1aff"].map(String::from));
        for theme in builtins() {
            for a in &accents {
                for compositor in [true, false] {
                    let p = resolve(&theme, a, compositor);
                    check(&p, &theme.id, a);
                }
            }
        }
    }

    #[test]
    fn translucency_falls_back_and_stays_readable() {
        let t = builtin("translucent").unwrap();
        let p = resolve(&t, "theme", true);
        assert!(p.window_alpha < 1.0 && p.sidebar_alpha < 1.0);
        // Layers stacked on the window add up to the effective opacity.
        let stacked = 1.0 - (1.0 - p.window_alpha) * (1.0 - p.layer_alpha(p.sidebar_alpha));
        assert!((stacked - p.sidebar_alpha).abs() < 1e-9);
        assert!(p.window_alpha <= 0.9, "visibly translucent: {}", p.window_alpha);
        assert!(p.css().contains("rgba("));
        let solid = resolve(&t, "theme", false);
        assert_eq!((solid.window_alpha, solid.view_alpha, solid.sidebar_alpha), (1.0, 1.0, 1.0));
        assert!(!solid.css().contains("rgba(") || !solid.css().contains("--window-bg-color: rgba"));
    }

    #[test]
    fn high_contrast_is_seven_to_one() {
        let p = resolve(&builtin("high-contrast").unwrap(), "yellow", true);
        assert!(contrast(p.window_fg, p.window_bg) >= 7.0);
        assert!(contrast(p.accent_color, p.view_bg) >= 7.0);
        assert!(p.css().contains("outline-width: 3px"));
    }

    #[test]
    fn user_themes_load_and_get_fixed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("solarized.toml"),
            "name = \"Solarized Dark\"\ndark = true\n[colors]\nwindow_bg = \"#002b36\"\nwindow_fg = \"#839496\"\naccent = \"#268bd2\"\ndim_fg = \"#586e75\"\n",
        )
        .unwrap();
        std::fs::write(d.path().join("broken.toml"), "name = \"Oops\"\n").unwrap();
        std::fs::write(d.path().join("badcolor.toml"), "name = \"B\"\ndark = false\n[colors]\nwindow_bg = \"red\"\nwindow_fg = \"#000\"\n").unwrap();
        let (ok, errors) = user_themes(d.path());
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].id, "user:solarized");
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|e| e.contains("isn't a colour")));
        // Solarized's dim #586e75 is too faint on #002b36; it gets lifted.
        let p = resolve(&ok[0], "theme", true);
        check(&p, "solarized", "theme");
        assert_ne!(p.dim_fg.hex(), "#586e75");
    }

    #[test]
    fn css_overrides_libadwaita_variables() {
        let css = resolve(&builtin("nord").unwrap(), "teal", true).css();
        for v in ["--window-bg-color", "--accent-bg-color", "--accent-color", "--sidebar-bg-color", "--tuck-dim-fg"] {
            assert!(css.contains(v), "{v}");
        }
        assert_eq!(CHOICES.len(), 11);
    }
}
