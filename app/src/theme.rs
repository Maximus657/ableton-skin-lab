//! Colour themes: the palette of a picture (k-means in OKLab) and Live `.ask` files built from it.
//! Port of `прототип/gen_theme.py`.

use crate::config::{fnv, Colors, Fit, Layer};
use image::{DynamicImage, ImageReader};
use std::collections::HashSet;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};

/// Live's stock theme every skin is built from.
pub const BASE_NAME: &str = "Default Dark Neutral Medium.ask";

const ACCENT1: [&str; 1] = ["ffad56"]; // Live's orange
const ACCENT2: [&str; 5] = ["03c3d5", "007383", "009aac", "8cffff", "79bdc7"]; // cyan family
const SELECTION: [&str; 4] = ["b0ddeb", "637e86", "7a959e", "b3d4e5"];

fn to_linear(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_gamma(c: f64) -> f64 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.abs().powf(1.0 / 2.4) - 0.055 }
}

pub fn oklab(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(to_linear);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_rgb([ll, a, b]: [f64; 3]) -> [f64; 3] {
    let l = (ll + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (ll - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (ll - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
    .map(to_gamma)
}

/// OKLCH -> 8-bit sRGB, shrinking chroma until the colour fits the gamut.
pub fn lch(l: f64, mut c: f64, h: f64) -> [u8; 3] {
    let mut rgb = [0.0; 3];
    for _ in 0..40 {
        rgb = oklab_to_rgb([l, c * h.cos(), c * h.sin()]);
        if rgb.iter().all(|&v| (-1e-4..=1.0 + 1e-4).contains(&v)) {
            break;
        }
        c *= 0.92;
    }
    rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// (lightness, chroma, hue in radians)
pub fn to_lch(rgb: [u8; 3]) -> (f64, f64, f64) {
    let [l, a, b] = oklab(rgb.map(|v| v as f64 / 255.0));
    (l, a.hypot(b), b.atan2(a))
}

fn hue_dist(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % (2.0 * PI);
    d.min(2.0 * PI - d)
}

/// Opens a picture the way it is meant to be seen (EXIF rotation applied).
pub fn open_image(path: &Path) -> Result<DynamicImage, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let reader = ImageReader::open(path).map_err(|e| err(&e))?.with_guessed_format().map_err(|e| err(&e))?;
    let mut decoder = reader.into_decoder().map_err(|e| err(&e))?;
    use image::ImageDecoder;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| err(&e))?;
    img.apply_orientation(orientation);
    Ok(img)
}

fn dist2(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Darker backgrounds: dark greys lose up to 45 % of their lightness, light ones (text) keep it.
fn darken(l: f64, depth: f64) -> f64 {
    l * (1.0 - 0.45 * depth * smoothstep(0.62, 0.45, l))
}

/// The lightness a grey had before `darken`.
pub fn undarken(l: f64, depth: f64) -> f64 {
    let (mut lo, mut hi) = (l, (l / 0.55).min(1.0));
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if darken(mid, depth) < l {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

/// A grey of Live's stock theme (OKLab lightness `l`) in the colours `c`.
pub fn grey(l: f64, c: &Colors) -> [u8; 3] {
    let l = darken(l, c.depth.clamp(0.0, 1.0) as f64);
    let fade = (l / 0.25).min(1.0) * ((1.0 - l) / 0.25).min(1.0);
    lch(l, c.tint as f64 * fade, (c.hue as f64).to_radians())
}

/// Interface colours that suit a picture.
pub fn colors_from_image(img: &DynamicImage, from: Option<String>) -> Colors {
    let small = img.thumbnail(160, 160).to_rgb8();
    let px: Vec<[f64; 3]> = small.pixels().map(|p| oklab(p.0.map(|v| v as f64 / 255.0))).collect();
    if px.is_empty() {
        return Colors::default();
    }
    const K: usize = 7;
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    // k-means++ seeding
    let mut cent = vec![px[(rnd() * px.len() as f64) as usize % px.len()]];
    while cent.len() < K {
        let d: Vec<f64> = px.iter().map(|p| cent.iter().map(|c| dist2(p, c)).fold(f64::MAX, f64::min)).collect();
        let total: f64 = d.iter().sum();
        let mut r = rnd() * total;
        let mut pick = px.len() - 1;
        for (i, &di) in d.iter().enumerate() {
            if r < di {
                pick = i;
                break;
            }
            r -= di;
        }
        cent.push(px[pick]);
    }
    let mut label = vec![0usize; px.len()];
    for _ in 0..20 {
        for (i, p) in px.iter().enumerate() {
            label[i] = (0..K).min_by(|&a, &b| dist2(p, &cent[a]).total_cmp(&dist2(p, &cent[b]))).unwrap_or(0);
        }
        let mut sum = [[0.0f64; 3]; K];
        let mut n = [0usize; K];
        for (p, &l) in px.iter().zip(&label) {
            n[l] += 1;
            for c in 0..3 {
                sum[l][c] += p[c];
            }
        }
        for j in 0..K {
            if n[j] > 0 {
                cent[j] = sum[j].map(|s| s / n[j] as f64);
            }
        }
    }
    let mut w = [0.0f64; K];
    for &l in &label {
        w[l] += 1.0 / px.len() as f64;
    }
    let chroma: Vec<f64> = cent.iter().map(|c| c[1].hypot(c[2])).collect();
    let hue: Vec<f64> = cent.iter().map(|c| c[2].atan2(c[1])).collect();

    // black-and-white art has no hue worth taking: greys stay neutral and the accents go silver
    let mono = (0..K).filter(|&i| w[i] > 0.02).map(|i| chroma[i]).fold(0.0, f64::max) < 0.035;
    // mood hue: chroma-weighted circular mean of the whole picture
    let (sy, sx) = (0..K).fold((0.0, 0.0), |(y, x), i| (y + w[i] * chroma[i] * hue[i].sin(), x + w[i] * chroma[i] * hue[i].cos()));
    let mood = sy.atan2(sx);
    let tint = if mono { 0.0 } else { (0.012 + 0.25 * (0..K).map(|i| w[i] * chroma[i]).sum::<f64>()).min(0.045) };
    let mut vivid: Vec<usize> = (0..K).collect();
    vivid.sort_by(|&a, &b| (chroma[b] * w[b].sqrt()).total_cmp(&(chroma[a] * w[a].sqrt())));
    let a1 = vivid[0];
    let a2 = vivid[1..].iter().copied().find(|&i| hue_dist(hue[i], hue[a1]) > 40f64.to_radians() && chroma[i] > 0.04);
    let (h1, c1) = (hue[a1], chroma[a1].max(0.12));
    let (h2, c2) = match a2 {
        Some(i) => (hue[i], chroma[i].max(0.10)),
        None => (h1 + 150f64.to_radians(), 0.12),
    };
    let mut order: Vec<usize> = (0..K).collect();
    order.sort_by(|&a, &b| w[b].total_cmp(&w[a]));
    Colors {
        hue: mood.to_degrees().rem_euclid(360.0) as f32,
        tint: tint as f32,
        accent1: if mono { lch(0.9, 0.0, 0.0) } else { lch(0.75, c1, h1) },
        accent2: if mono { lch(0.75, 0.0, 0.0) } else { lch(0.7, c2, h2) },
        // black-and-white art: a black-and-white Live, a shade darker
        depth: if mono { 0.5 } else { 0.0 },
        saturation: if mono { 0.0 } else { 1.0 },
        manual: false,
        palette: order
            .iter()
            .map(|&i| (oklab_to_rgb(cent[i]).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8), w[i] as f32))
            .collect(),
        from,
    }
}

/// How a picture first goes into a panel of the given aspect (width / height).
pub fn suggest_layer(img: &DynamicImage, panel_aspect: f32) -> Layer {
    let aspect = img.width() as f32 / img.height().max(1) as f32;
    let small = img.thumbnail(96, 96).to_rgba8();
    let opaque: Vec<_> = small.pixels().filter(|p| p[3] > 128).collect();
    let n = opaque.len().max(1) as f32;
    let mean = opaque.iter().map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).sum::<f32>() / n / 255.0;
    let grey = opaque.iter().all(|p| {
        let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
        mx - mn < 40
    });
    // a drawing on black: most of its border is near black
    let (sw, sh) = small.dimensions();
    let border: Vec<bool> = small
        .enumerate_pixels()
        .filter(|(x, y, p)| p[3] > 128 && (*x == 0 || *y == 0 || *x + 1 == sw || *y + 1 == sh))
        .map(|(_, _, p)| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) < 0.2 * 255.0)
        .collect();
    let dark_edges = border.len() > 8 && border.iter().filter(|d| **d).count() * 5 > border.len() * 2;
    let mut layer = Layer::default();
    if (opaque.len() as f32) < 0.9 * (small.width() * small.height()) as f32 {
        // a cut-out (transparent background): a sticker in the corner
        layer.fit = Fit::Free;
        layer.zoom = 0.8;
        layer.anchor = [0.95, 1.0];
        layer.brightness = 1.0;
        layer.soft = 0.0;
    } else if aspect < 0.45 * panel_aspect {
        // a picture much narrower than its panel: keep all of it, a bit right of centre
        layer.fit = Fit::Contain;
        layer.anchor = [0.67, 0.5];
    }
    if grey && mean > 0.6 {
        // pencil or ink on white paper: draw the lines in chalk
        layer.ink = true;
        layer.brightness = 0.85;
    } else if layer.fit != Fit::Free {
        // the picture sits behind text: keep its average around a dark grey
        layer.brightness = (0.30 / mean.max(0.05)).clamp(0.3, 1.0);
    }
    if dark_edges && !layer.ink {
        // its black background takes the panel's colour instead of showing as a dark patch
        layer.screen = true;
        layer.brightness = layer.brightness.min(0.85);
    }
    layer
}

/// A Live theme built from the stock one.
#[derive(Clone, Debug)]
pub struct Built {
    pub text: String,
    /// panel background: the overlay finds it by exact colour
    pub key: [u8; 3],
    pub hash: u64,
}

pub fn build(base: &str, c: &Colors) -> Built {
    let (_, c1, h1) = to_lch(c.accent1);
    let (_, c2, h2) = to_lch(c.accent2);
    let (mono1, mono2) = (c1 < 0.03, c2 < 0.03);
    let sat = c.saturation.clamp(0.0, 1.0) as f64;

    struct Entry {
        line: usize,
        name: String,
        at: usize,
        len: usize,
        rgb: [u8; 3],
        alpha: String,
    }
    let mut lines: Vec<String> = base.lines().map(String::from).collect();
    let mut entries = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(v) = line.find("Value=\"#") else { continue };
        let at = v + 8;
        let len = line[at..].find('"').unwrap_or(0);
        let hex = &line[at..at + len];
        if len < 6 || !hex.is_ascii() {
            continue;
        }
        let byte = |k: usize| u8::from_str_radix(&hex[k..k + 2], 16).unwrap_or(0);
        let name = line.trim_start().trim_start_matches('<').split_whitespace().next().unwrap_or("").to_string();
        entries.push(Entry { line: i, name, at, len, rgb: [byte(0), byte(2), byte(4)], alpha: hex[6..].to_string() });
    }
    for e in &mut entries {
        let hex = format!("{:02x}{:02x}{:02x}", e.rgb[0], e.rgb[1], e.rgb[2]);
        let (l, cc, h) = to_lch(e.rgb);
        e.rgb = if ACCENT1.contains(&hex.as_str()) {
            if mono1 { lch(l.max(0.9), 0.0, 0.0) } else { lch(l.max(0.72), c1, h1) }
        } else if ACCENT2.contains(&hex.as_str()) {
            if mono2 { lch(l, 0.0, 0.0) } else { lch(l, cc.max(c2) * 0.9, h2) }
        } else if SELECTION.contains(&hex.as_str()) {
            if mono2 { lch(l, 0.0, 0.0) } else { lch(l, cc, h2) }
        } else if cc < 0.02 {
            // grey: keep the lightness, take the mood (fading out towards black and white)
            grey(l, c)
        } else if sat < 1.0 {
            lch(l, cc * sat, h)
        } else {
            continue;
        };
    }
    // the overlay finds the background by exact colour, so nobody else may share it
    let mut key = [0x36, 0x36, 0x36];
    if let Some(sb) = entries.iter().position(|e| e.name == "SurfaceBackground") {
        let taken: HashSet<[u8; 3]> = entries.iter().enumerate().filter(|(i, _)| *i != sb).map(|(_, e)| e.rgb).collect();
        key = entries[sb].rgb;
        for _ in 0..64 {
            if !taken.contains(&key) {
                break;
            }
            key[2] = if key[2] < 255 { key[2] + 1 } else { key[2] - 1 };
        }
        entries[sb].rgb = key;
    }
    for e in &entries {
        let v = format!("{:02x}{:02x}{:02x}{}", e.rgb[0], e.rgb[1], e.rgb[2], e.alpha);
        lines[e.line].replace_range(e.at..e.at + e.len, &v);
    }
    let text = lines.join("\n") + "\n";
    let hash = fnv(text.as_bytes());
    Built { text, key, hash }
}

/// Where Live keeps its themes, newest install first.
pub fn theme_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let state: crate::config::State = crate::config::load(&crate::config::state_path());
    if let Some(d) = state.themes_dir {
        out.push(PathBuf::from(d));
    }
    for root in [r"C:\ProgramData\Ableton", r"C:\Program Files\Ableton"] {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("Live 1"))
            .map(|e| e.path().join("Resources").join("Themes"))
            .filter(|p| p.is_dir())
            .collect();
        found.sort();
        found.reverse();
        out.extend(found);
    }
    out
}

/// Live's stock dark theme (cached in our folder once found).
pub fn base_theme() -> Option<String> {
    let cached = crate::config::dir().join("base.ask");
    if let Ok(s) = std::fs::read_to_string(&cached) {
        return Some(s);
    }
    for d in theme_dirs() {
        if let Ok(s) = std::fs::read_to_string(d.join(BASE_NAME)) {
            let _ = std::fs::write(&cached, &s);
            return Some(s);
        }
    }
    None
}
