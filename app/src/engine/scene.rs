//! The active skin, ready to draw: its Live theme, its pictures on the GPU and where each one
//! goes. Used by the overlay and by the editor's preview alike.

use super::gpu::{Draw, Gpu, Params, Pic, MAX_PANELS};
use crate::config::{Colors, Fit, Layer, Skin, ZONES};
use crate::log;
use crate::theme::{self, Built};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

#[derive(Default)]
pub struct Scene {
    pub built: Option<Built>,
    colors: Option<Colors>,
    pub layers: Vec<Layer>,
    backdrop: Vec<(String, f32)>,
    readable: f32,
    pub pics: HashMap<String, Pic>,
    /// pictures that would not open
    pub failed: HashSet<String>,
}

fn load_pic(gpu: &Gpu, path: &str) -> Result<Pic, String> {
    let img = theme::open_image(Path::new(path))?;
    // the shader blurs through mip levels, so 4K is plenty even for a 5K window
    let img = if img.width().max(img.height()) > 4096 {
        img.resize(4096, 4096, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut rgba = img.to_rgba8();
    let paper = paper(&rgba);
    // premultiplied, so that blurring and smaller mips mix colours by how opaque they are
    for p in rgba.pixels_mut() {
        let a = p[3] as u32;
        if a < 255 {
            for c in 0..3 {
                p[c] = ((p[c] as u32 * a + 127) / 255) as u8;
            }
        }
    }
    let mut pic = gpu.load_picture(&rgba).map_err(|e| e.to_string())?;
    pic.paper = paper;
    Ok(pic)
}

/// The paper of a drawing: the lowest lightness of its most common light tone, so that all of
/// it goes transparent in line-art mode, grey paper included.
fn paper(img: &image::RgbaImage) -> f32 {
    let small = image::imageops::thumbnail(img, 64, 64);
    let mut hist = [0u32; 32];
    for p in small.pixels().filter(|p| p[3] >= 128) {
        let l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0;
        if l >= 0.45 {
            hist[((l - 0.45) / 0.55 * 31.99) as usize] += 1;
        }
    }
    match hist.iter().enumerate().max_by_key(|(_, n)| **n) {
        Some((i, n)) if *n > 0 => 0.45 + i as f32 / 32.0 * 0.55,
        _ => 1.0,
    }
}

/// Where a `pw`x`ph` picture is drawn in a panel `r` (x, y, w, h in frame pixels).
pub fn place(r: [f32; 4], layer: &Layer, pw: f32, ph: f32) -> [f32; 4] {
    let (pw, ph) = if layer.rotate % 2 == 1 { (ph, pw) } else { (pw, ph) };
    let fit = match layer.fit {
        Fit::Cover => (r[2] / pw).max(r[3] / ph),
        Fit::Contain | Fit::Free => (r[2] / pw).min(r[3] / ph),
    };
    let s = fit * layer.zoom.max(0.02);
    let (dw, dh) = (pw * s, ph * s);
    [r[0] + (r[2] - dw) * layer.anchor[0], r[1] + (r[3] - dh) * layer.anchor[1], dw, dh]
}

fn layer_params(r: [f32; 4], layer: &Layer, pic: &Pic, key: [u8; 3]) -> Params {
    let soft = if layer.fit == Fit::Cover && layer.zoom >= 1.0 { 0.0 } else { layer.soft };
    // a whole panel to fill around a picture that does not reach its edges
    let smear = if layer.fit == Fit::Contain { (r[3] * 0.03).max(8.0) } else { 0.0 };
    let ink = layer.ink_color.map(|v| v as f32 / 255.0);
    let backdrop = layer.zone == "window" && layer.fit != Fit::Free;
    let screen = layer.screen && !layer.ink;
    let k = if screen { key.map(|v| v as f32 / 255.0) } else { [0.0; 3] };
    Params {
        key: [k[0], k[1], k[2], 0.0],
        misc: [0.0, if backdrop { 1.0 } else { 0.0 }, 0.0, 0.0],
        place: place(r, layer, pic.w as f32, pic.h as f32),
        look: [layer.brightness, layer.blur, layer.opacity, if layer.ink { 1.0 } else { 0.0 }],
        ink: [ink[0], ink[1], ink[2], soft],
        pic: [pic.w as f32, pic.h as f32, pic.mips as f32, smear],
        flags: [if layer.mirror { 1.0 } else { 0.0 }, pic.paper, (layer.rotate % 4) as f32, if screen { 1.0 } else { 0.0 }],
        ..Default::default()
    }
}

impl Scene {
    /// Takes over `skin`: builds its theme, loads new pictures, drops unused ones.
    pub fn prepare(&mut self, gpu: &Gpu, skin: Option<&Skin>) {
        let colors = skin.map(|s| s.colors.clone());
        if colors != self.colors || self.built.is_none() {
            self.built = match (&colors, theme::base_theme()) {
                (Some(c), Some(base)) => Some(theme::build(&base, c)),
                (Some(_), None) => {
                    log!("Live's stock theme '{}' not found", theme::BASE_NAME);
                    None
                }
                _ => None,
            };
            self.colors = colors;
        }
        self.layers = skin.map(|s| s.layers.clone()).unwrap_or_default();
        self.backdrop = skin.map(|s| s.panels.iter().map(|(k, p)| (k.clone(), p.backdrop)).collect()).unwrap_or_default();
        self.readable = skin.map(|s| s.readable).unwrap_or(0.0);
        let wanted: HashSet<String> = self.layers.iter().map(|l| l.picture.clone()).filter(|p| !p.is_empty()).collect();
        self.pics.retain(|k, _| wanted.contains(k));
        self.failed.retain(|k| wanted.contains(k));
        for path in wanted {
            if self.pics.contains_key(&path) || self.failed.contains(&path) {
                continue;
            }
            let t0 = Instant::now();
            match load_pic(gpu, &path) {
                Ok(p) => {
                    log!("picture {}x{} in {} ms: {path}", p.w, p.h, t0.elapsed().as_millis());
                    self.pics.insert(path, p);
                }
                Err(e) => {
                    log!("picture failed: {e}");
                    self.failed.insert(path);
                }
            }
        }
    }

    pub fn has_pictures(&self) -> bool {
        self.layers.iter().any(|l| !l.hidden && self.pics.contains_key(&l.picture))
    }

    fn backdrop(&self, zone: &str) -> f32 {
        self.backdrop.iter().find(|(k, _)| k == zone).map(|(_, b)| *b).unwrap_or(1.0)
    }

    /// The pictures to draw into a frame of `size` with panel background `key`, bottom first.
    /// `rect` gives a panel's rectangle in frame pixels (x, y, w, h), if Live shows it.
    pub fn draws(&self, size: (u32, u32), key: [u8; 3], rect: impl Fn(&str) -> Option<[f32; 4]>) -> Vec<Draw> {
        let full = [0.0, 0.0, size.0 as f32, size.1 as f32];
        let zone_rect = |id: &str| if id == "window" { Some(full) } else { rect(id) }.filter(|r| r[2] > 0.0 && r[3] > 0.0);
        // panels that let less of the whole-window backgrounds through
        let mut panels = [[0.0; 4]; MAX_PANELS];
        let mut backdrop = [[1.0, 0.0, 0.0, 0.0]; MAX_PANELS];
        let mut n = 0;
        for id in ZONES.iter().filter(|id| **id != "window") {
            let b = self.backdrop(id);
            if let (true, Some(r)) = (b < 1.0 && n < MAX_PANELS, zone_rect(id)) {
                panels[n] = r;
                backdrop[n][0] = b.max(0.0);
                n += 1;
            }
        }
        self.layers
            .iter()
            .filter(|l| !l.hidden)
            .filter_map(|l| {
                let r = zone_rect(&l.zone)?;
                let pic = self.pics.get(&l.picture)?;
                let mut p = layer_params(r, l, pic, key);
                p.panels = panels;
                p.backdrop = backdrop;
                let clip = [r[0].round() as i32, r[1].round() as i32, (r[0] + r[2]).round() as i32, (r[1] + r[3]).round() as i32];
                Some(Draw { p, srv: pic.srv.clone(), clip })
            })
            .collect()
    }

    /// Parameters of the final step: the panel background colour of the frame, and how much the
    /// pictures give way around Live's text.
    pub fn combine_params(&self, key: [u8; 3]) -> Params {
        let k = key.map(|v| v as f32 / 255.0);
        Params { key: [k[0], k[1], k[2], 0.0], misc: [self.readable.clamp(0.0, 1.0), 0.0, 0.0, 1.0], ..Default::default() }
    }
}
