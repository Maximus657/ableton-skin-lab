//! The skin over a frame of Live, rendered off-screen with the overlay's own shader: the editor's
//! preview, and `--preview` for checking a skin without touching the screen.

use crate::config::{self, Colors, Skin, Snapshot};
use crate::engine::{gpu::Gpu, scene::Scene};
use crate::theme;
use std::collections::{BTreeMap, HashMap};

pub struct Preview {
    gpu: Gpu,
    scene: Scene,
    snap: Option<Snapshot>,
}

impl Preview {
    pub fn new() -> Result<Preview, String> {
        Ok(Preview { gpu: Gpu::new(None).map_err(|e| e.to_string())?, scene: Scene::default(), snap: None })
    }

    pub fn set_frame(&mut self, snap: Snapshot, bgra: &[u8]) -> Result<(), String> {
        let [w, h] = snap.size;
        if bgra.len() != (w * h * 4) as usize {
            return Err("снимок Live повреждён".into());
        }
        self.gpu.set_frame(w, h, bgra).map_err(|e| e.to_string())?;
        self.snap = Some(snap);
        Ok(())
    }

    pub fn frame_size(&self) -> Option<[u32; 2]> {
        self.snap.as_ref().map(|s| s.size)
    }

    pub fn set_skin(&mut self, skin: &Skin) {
        self.scene.prepare(&self.gpu, Some(skin));
    }

    /// The theme background of the current skin.
    pub fn skin_key(&self) -> Option<[u8; 3]> {
        self.scene.built.as_ref().map(|b| b.key)
    }

    /// Size of a loaded picture (it may have been scaled down, keeping its shape).
    pub fn pic_size(&self, path: &str) -> Option<(f32, f32)> {
        self.scene.pics.get(path).map(|p| (p.w as f32, p.h as f32))
    }

    pub fn failed(&self, path: &str) -> bool {
        self.scene.failed.contains(path)
    }

    /// RGBA, `scale` times smaller than the frame.
    pub fn render(&mut self, scale: f32) -> Result<image::RgbaImage, String> {
        let snap = self.snap.as_ref().ok_or("нет снимка Live")?;
        let zones = &snap.zones;
        let draws = self.scene.draws((snap.size[0], snap.size[1]), snap.key, |id| {
            zones.get(id).map(|r| [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32])
        });
        let p = self.scene.combine_params(snap.key);
        let (w, h, px) = self.gpu.preview(&draws, &p, scale).map_err(|e| e.to_string())?;
        image::RgbaImage::from_raw(w, h, px).ok_or_else(|| "пустой кадр".into())
    }
}

/// The engine's last snapshot of Live, if any. A frame in a theme that is not ours gets its
/// panel colour from the pixels.
pub fn load_snapshot() -> Option<(Snapshot, Vec<u8>)> {
    let (json, raw) = config::snapshot_paths();
    let mut snap: Snapshot = serde_json::from_slice(&std::fs::read(json).ok()?).ok()?;
    let px = std::fs::read(raw).ok()?;
    if px.len() != (snap.size[0] * snap.size[1] * 4) as usize {
        return None;
    }
    if !snap.theme_ok {
        snap.key = dominant(&snap, &px);
        snap.colors = None;
    }
    Some((snap, px))
}

/// The panel colour of a frame: its most common colour in the arrangement.
fn dominant(snap: &Snapshot, px: &[u8]) -> [u8; 3] {
    let [w, h] = snap.size;
    let r = snap.zones.get("lanes").copied().unwrap_or([0, 0, w as i32, h as i32]);
    let mut count: HashMap<[u8; 3], u32> = HashMap::new();
    for y in (r[1].max(0)..(r[1] + r[3]).min(h as i32)).step_by(3) {
        for x in (r[0].max(0)..(r[0] + r[2]).min(w as i32)).step_by(3) {
            let i = ((y as u32 * w + x as u32) * 4) as usize;
            *count.entry([px[i + 2], px[i + 1], px[i]]).or_default() += 1;
        }
    }
    count.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c).unwrap_or([54, 54, 54])
}

/// What decides how a skin theme looks (its pixels, its panel colour). Never 0.
pub fn theme_sig(c: &Colors) -> u64 {
    let mut b = Vec::with_capacity(24);
    for v in [c.hue, c.tint, c.depth, c.saturation] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&c.accent1);
    b.extend_from_slice(&c.accent2);
    config::fnv(&b) | 1
}

/// Live's panels in the client area of a maximised window 3440 pixels wide.
fn panels_3440() -> BTreeMap<String, [i32; 4]> {
    [
        ("browser", [9, 41, 463, 1070]),
        ("lanes", [481, 59, 2374, 1052]),
        ("tracks", [2855, 59, 291, 1052]),
        ("device", [2, 1119, 3144, 190]),
        ("side", [3155, 41, 276, 1295]),
    ]
    .into_iter()
    .map(|(id, r)| (id.to_string(), r))
    .collect()
}

fn to_bgra(img: image::RgbaImage) -> Vec<u8> {
    let mut px = img.into_raw();
    for c in px.chunks_exact_mut(4) {
        c.swap(0, 2);
    }
    px
}

/// A frame to show until Live has been seen: a real Live 12 window (3440 wide) in a theme with
/// panel colour #402f44, to be repainted into the skin's theme.
pub fn stand_in() -> (Snapshot, Vec<u8>) {
    let img = image::load_from_memory(include_bytes!("../assets/live.png")).map(|i| i.to_rgba8()).unwrap_or_else(|_| image::RgbaImage::new(1, 1));
    let (w, h) = img.dimensions();
    let zones = if w == 3440 { panels_3440() } else { BTreeMap::new() };
    (Snapshot { size: [w, h], key: [0x40, 0x2f, 0x44], theme_ok: false, colors: None, zones }, to_bgra(img))
}

/// Repaints a frame of Live (BGRA) from the theme it shows, with background `from_key` and
/// colours `from` if known, into the theme of the colours `to` with background `to_key`: greys
/// by their lightness, other colours by `to.saturation`. Close to what Live shows once it has
/// switched themes.
pub fn retheme(bgra: &mut [u8], from_key: [u8; 3], from: Option<&Colors>, to: &Colors, to_key: [u8; 3]) {
    let depth = from.map(|c| c.depth as f64).unwrap_or(0.0);
    // how tinted the old greys are, to tell them from real colours
    let grey_c = from.map(|c| c.tint as f64).unwrap_or_else(|| theme::to_lch(from_key).1) + 0.03;
    let sat = to.saturation.clamp(0.0, 1.0) as f64;
    let k = from_key.map(|v| v as i32);
    let mut memo: HashMap<[u8; 3], [u8; 3]> = HashMap::new();
    for px in bgra.chunks_exact_mut(4) {
        let rgb = [px[2], px[1], px[0]];
        let d = [rgb[0] as i32 - k[0], rgb[1] as i32 - k[1], rgb[2] as i32 - k[2]];
        let out = if d.iter().all(|v| v.abs() < 14) {
            // the panel background and what is drawn just off it keep their difference
            [0, 1, 2].map(|i| (to_key[i] as i32 + d[i]).clamp(0, 255) as u8)
        } else {
            *memo.entry(rgb).or_insert_with(|| {
                let (l, c, h) = theme::to_lch(rgb);
                if c < grey_c {
                    theme::grey(theme::undarken(l, depth), to)
                } else {
                    theme::lch(l, c * sat, h)
                }
            })
        };
        px[..3].copy_from_slice(&[out[2], out[1], out[0]]);
    }
}

/// `--preview out.png [frame.png #key]`: the active skin over the engine's last snapshot of Live
/// (or the stand-in), or over a screenshot of Live's client area taken in a theme with panel
/// colour `key`, repainted into the skin's own theme as Live will show it.
pub fn cli(args: &[String]) -> Result<(), String> {
    let out = args.first().ok_or("нужен путь для картинки")?;
    let cfg: config::Config = config::load(&config::config_path());
    let skin = cfg.active_skin().cloned().ok_or("нет скинов")?;
    let mut pv = Preview::new()?;
    pv.set_skin(&skin);
    let (mut snap, mut px) = match (args.get(1), args.get(2)) {
        (Some(frame), Some(key)) => {
            let img = image::open(frame).map_err(|e| e.to_string())?.to_rgba8();
            let k = u32::from_str_radix(key.trim_start_matches('#'), 16).map_err(|e| e.to_string())?;
            let (w, h) = img.dimensions();
            let zones = if w == 3440 { panels_3440() } else { BTreeMap::new() };
            let key = [(k >> 16) as u8, (k >> 8) as u8, k as u8];
            (Snapshot { size: [w, h], key, theme_ok: false, colors: None, zones }, to_bgra(img))
        }
        _ => load_snapshot().unwrap_or_else(stand_in),
    };
    if let Some(to) = pv.skin_key() {
        let shown = snap.colors.as_ref().filter(|_| snap.theme_ok).map(theme_sig);
        if shown != Some(theme_sig(&skin.colors)) {
            retheme(&mut px, snap.key, snap.colors.as_ref(), &skin.colors, to);
            snap.key = to;
        }
    }
    pv.set_frame(snap, &px)?;
    pv.render(1.0)?.save(out).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Layer;

    /// What the clip editor's rows are made of, in Live's stock theme.
    const STOCK: &str = r##"<SurfaceBackground Value="#363636" />
<DetailViewBackground Value="#3e3e3e" />
<DetailGridTiles Value="#0a0a0a19" />
<GridLineBase Value="#06060654" />
<MidiEditorBlackKeyBackground Value="#07070726" />
"##;

    fn entry(theme: &str, name: &str) -> ([f32; 3], f32) {
        let line = theme.lines().find(|l| l.contains(name)).expect(name);
        let hex = line.split("Value=\"#").nth(1).and_then(|v| v.split('"').next()).expect(name);
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect(name) as f32;
        ([byte(0), byte(2), byte(4)], if hex.len() > 6 { byte(6) / 255.0 } else { 1.0 })
    }

    /// The picture showed through the rows of the piano roll in patches, every other bar.
    #[test]
    fn a_picture_stays_out_of_the_clip_editors_rows() {
        const BLOCK: u32 = 8;
        let path = std::env::temp_dir().join(format!("skinlab-test-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(BLOCK, BLOCK, image::Rgba([255, 255, 255, 255])).save(&path).unwrap();
        let mut pv = Preview::new().unwrap();
        for (hue, tint, depth) in [(0.0, 0.0, 0.0), (303.0, 0.0264, 0.0), (200.0, 0.06, 0.5), (30.0, 0.03, 1.0)] {
            let colors = Colors { hue, tint, depth, ..Colors::default() };
            let t = theme::build(STOCK, &colors);
            // the rows with everything Live can draw on them, in any combination
            let mut mixes = vec![entry(&t.text, "DetailViewBackground").0];
            for name in ["DetailGridTiles", "MidiEditorBlackKeyBackground", "GridLineBase"] {
                let (c, a) = entry(&t.text, name);
                for m in mixes.clone() {
                    mixes.push([0, 1, 2].map(|i| m[i] * (1.0 - a) + c[i] * a));
                }
            }
            // a frame of blocks: the panel colour, then each mix
            let (w, h) = ((mixes.len() as u32 + 1) * BLOCK, BLOCK);
            let colour = |x: u32| match (x / BLOCK) as usize {
                0 => t.key,
                n => mixes[n - 1].map(|v| v.round() as u8),
            };
            let px: Vec<u8> = (0..w * h).flat_map(|i| colour(i % w).into_iter().rev().chain([255])).collect();
            let zones = BTreeMap::from([("lanes".to_string(), [0, 0, w as i32, h as i32])]);
            let snap = Snapshot { size: [w, h], key: t.key, theme_ok: true, colors: Some(colors.clone()), zones };
            let layer = Layer { picture: path.to_string_lossy().into_owned(), zone: "lanes".into(), brightness: 1.0, soft: 0.0, ..Layer::default() };
            pv.set_skin(&Skin { colors, layers: vec![layer], readable: 0.0, ..Skin::default() });
            pv.set_frame(snap, &px).unwrap();
            let out = pv.render(1.0).unwrap();
            let shown = |x: u32| &out.get_pixel(x, BLOCK / 2).0[..3];
            assert_eq!(shown(BLOCK / 2), [255, 255, 255], "the panel shows the picture");
            for b in 1..=mixes.len() as u32 {
                let x = b * BLOCK + BLOCK / 2;
                assert_eq!(shown(x), colour(x), "hue {hue} depth {depth}, mix {b}");
            }
        }
        let _ = std::fs::remove_file(path);
    }
}
