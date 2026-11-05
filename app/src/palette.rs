//! The editor's own colours, made from the active skin's: panels take the skin's grey tint, buttons
//! and outlines its accents, and the whole window sits on a soft glow of the preview. So the app
//! wears the colours of the pictures loaded into it, and fades to the next ones.

use crate::config::{self, Colors};
use crate::theme;
use eframe::egui::{self, pos2, Color32, ColorImage, Pos2, Rect, Stroke, TextureHandle, TextureOptions};
use std::cell::Cell;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

/// How much each panel covers the glow behind it.
pub const TOP_ALPHA: f32 = 0.92;
pub const SIDE_ALPHA: f32 = 0.88;
pub const STAGE_ALPHA: f32 = 0.6;

const FADE_SECS: f32 = 0.3;
const GLOW_W: usize = 16;
const GLOW_H: usize = 9;

macro_rules! palette {
    ($($name:ident),+ $(,)?) => {
        #[derive(Clone, Copy, PartialEq)]
        pub struct Pal { $(pub $name: Color32),+ }

        impl Pal {
            fn mix(self, to: Pal, k: f32) -> Pal {
                Pal { $($name: self.$name.lerp_to_gamma(to.$name, k)),+ }
            }
        }
    };
}

palette!(
    accent, accent2, // text and outlines on the panels
    fill, on_fill,   // main buttons and their text
    select,          // selected rows
    top, side, stage, window, deep, row, row_hover, line, knob, hover, hover_knob,
    text, strong, title, weak,
);

impl Pal {
    /// Stays readable whatever a picture brings.
    pub fn of(c: &Colors) -> Pal {
        // a skin with no colours of its own yet: the app's purple, not Live's orange
        let brand = Colors { hue: 290.0, tint: 0.012, accent1: [160, 120, 255], accent2: [90, 200, 255], ..Colors::default() };
        let c = if *c == Colors::default() { &brand } else { c };
        let (l1, c1, h1) = theme::to_lch(c.accent1);
        let (l2, c2, h2) = theme::to_lch(c.accent2);
        // untinted panels take a breath of the accent; black and white stays black and white
        let (ph, pc) = if c.tint > 0.006 { ((c.hue as f64).to_radians(), c.tint as f64) } else { (h1, 0.012 * (c1 / 0.08).min(1.0)) };
        let dark = 1.0 - 0.28 * c.depth.clamp(0.0, 1.0) as f64;
        let rgb = |l: f64, c: f64, h: f64| {
            let [r, g, b] = theme::lch(l, c, h);
            Color32::from_rgb(r, g, b)
        };
        let bg = |l: f64| rgb(l * dark, pc, ph);
        let fg = |l: f64| rgb(l, pc.min(0.015), ph);
        // a bright accent makes a bright button with dark text, a deep one keeps white text
        let bright = l1 >= 0.68;
        Pal {
            accent: rgb(l1.clamp(0.72, 0.90), c1, h1),
            accent2: rgb(l2.clamp(0.70, 0.88), c2, h2),
            fill: rgb(if bright { l1.clamp(0.74, 0.88) } else { l1.clamp(0.42, 0.52) }, c1, h1),
            on_fill: if bright { rgb(0.17, c1.min(0.04), h1) } else { rgb(0.98, c1.min(0.01), h1) },
            select: rgb(0.44, c1.min(0.14), h1),
            top: bg(0.195),
            side: bg(0.215),
            stage: bg(0.16),
            window: bg(0.235),
            deep: bg(0.17),
            row: bg(0.26),
            row_hover: bg(0.305),
            line: bg(0.285),
            knob: bg(0.33),
            hover: bg(0.345),
            hover_knob: bg(0.38),
            text: fg(0.87),
            strong: fg(0.92),
            title: fg(0.96),
            weak: rgb(0.71, pc.min(0.03), ph),
        }
    }
}

thread_local! {
    static PAL: Cell<Pal> = Cell::new(Pal::of(&Colors::default()));
}

/// The colours the window shows now.
pub fn pal() -> Pal {
    PAL.with(Cell::get)
}

/// Puts on `p`: for the code that draws, for egui's widgets and, with `caption`, for the title bar.
pub fn wear(ctx: &egui::Context, p: Pal, caption: bool) {
    PAL.with(|c| c.set(p));
    ctx.all_styles_mut(|s| {
        let v = &mut s.visuals;
        v.panel_fill = p.side;
        v.window_fill = p.window;
        v.window_stroke.color = p.line;
        v.extreme_bg_color = p.deep;
        v.faint_bg_color = p.row;
        v.hyperlink_color = p.accent;
        v.text_cursor.stroke.color = p.accent;
        let w = &mut v.widgets;
        w.noninteractive.weak_bg_fill = p.side;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
        w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        w.inactive.weak_bg_fill = p.line;
        w.inactive.bg_fill = p.knob;
        w.inactive.fg_stroke = Stroke::new(1.0, p.strong);
        w.hovered.weak_bg_fill = p.hover;
        w.hovered.bg_fill = p.hover_knob;
        w.hovered.bg_stroke = Stroke::new(1.0, p.accent);
        w.active.weak_bg_fill = p.select;
        w.active.bg_fill = p.select;
        w.open.weak_bg_fill = p.hover;
    });
    if caption {
        title_bar(&p);
    }
}

/// Sliders and picked items shine in the accent. But selected text keeps its light colour, so while
/// a text field has the keyboard the selection has to be the deep one.
pub fn light_selection(ctx: &egui::Context) {
    let p = pal();
    let focus = ctx.memory(|m| m.focused());
    let typing = focus.is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some());
    ctx.all_styles_mut(|s| {
        s.visuals.selection.bg_fill = if typing { p.select } else { p.fill };
        s.visuals.selection.stroke = Stroke::new(1.0, if typing { p.title } else { p.on_fill });
    });
}

/// The title bar in the top panel's colour. Windows 10 ignores this and keeps its own.
fn title_bar(p: &Pal) {
    let bgr = |c: Color32| c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16;
    unsafe {
        let Ok(h) = FindWindowW(config::EDITOR_CLASS, config::EDITOR_TITLE) else { return };
        for (attr, v) in [
            // light buttons on the dark bar
            (DWMWA_USE_IMMERSIVE_DARK_MODE, 1),
            (DWMWA_CAPTION_COLOR, bgr(p.top)),
            (DWMWA_TEXT_COLOR, bgr(p.text)),
            (DWMWA_BORDER_COLOR, bgr(p.line)),
        ] {
            let _ = DwmSetWindowAttribute(h, attr, &v as *const u32 as _, 4);
        }
    }
}

/// A change of colours under way.
pub struct Fade {
    from: Pal,
    to: Pal,
    t: f32,
}

impl Fade {
    pub fn new(p: Pal) -> Fade {
        Fade { from: p, to: p, t: 1.0 }
    }

    /// The colours for this frame, if they are still changing. Called with a new `to` every frame
    /// (a slider being dragged), it follows closely instead of restarting slowly.
    pub fn step(&mut self, to: Pal, dt: f32) -> Option<Pal> {
        if to != self.to {
            *self = Fade { from: pal(), to, t: 0.0 };
        }
        if self.t >= 1.0 {
            return None;
        }
        self.t = (self.t + dt / FADE_SECS).min(1.0);
        Some(self.from.mix(self.to, 1.0 - (1.0 - self.t).powi(3)))
    }
}

/// How a colour of the preview lights the backdrop: colour glows, grey stays dark.
fn glow_color(rgb: [u8; 3]) -> [u8; 3] {
    let (l, c, h) = theme::to_lch(rgb);
    let c = (c * 3.0).min(0.17);
    theme::lch(0.15 + 1.6 * c + 0.06 * l, c, h)
}

/// The preview shrunk to a few blurred colours.
pub fn glow_of(img: &image::RgbaImage) -> Vec<[f32; 3]> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut cells = vec![[0.0f32; 3]; GLOW_W * GLOW_H];
    for (i, cell) in cells.iter_mut().enumerate() {
        let (cx, cy) = (i % GLOW_W, i / GLOW_W);
        let (mut sum, mut n) = ([0u32; 3], 0u32);
        for y in (cy * h / GLOW_H..(cy + 1) * h / GLOW_H).step_by(3) {
            for x in (cx * w / GLOW_W..(cx + 1) * w / GLOW_W).step_by(3) {
                let p = img.get_pixel(x as u32, y as u32).0;
                for k in 0..3 {
                    sum[k] += p[k] as u32;
                }
                n += 1;
            }
        }
        *cell = glow_color(sum.map(|s| (s / n.max(1)) as u8)).map(|v| v as f32);
    }
    for _ in 0..2 {
        let old = cells.clone();
        for (i, cell) in cells.iter_mut().enumerate() {
            let (cx, cy) = ((i % GLOW_W) as i32, (i / GLOW_W) as i32);
            let mut sum = [0.0; 3];
            for (dx, dy) in (-1..=1).flat_map(|dy| (-1..=1).map(move |dx| (dx, dy))) {
                let x = (cx + dx).clamp(0, GLOW_W as i32 - 1) as usize;
                let y = (cy + dy).clamp(0, GLOW_H as i32 - 1) as usize;
                for k in 0..3 {
                    sum[k] += old[y * GLOW_W + x][k];
                }
            }
            *cell = sum.map(|s: f32| s / 9.0);
        }
    }
    cells
}

/// The window's backdrop: the blurred preview, drifting to the newest one.
pub struct Glow {
    tex: TextureHandle,
    now: Vec<[f32; 3]>,
    pub want: Vec<[f32; 3]>,
}

fn glow_image(cells: &[[f32; 3]]) -> ColorImage {
    let bytes: Vec<u8> = cells.iter().flatten().map(|v| v.round() as u8).collect();
    ColorImage::from_rgb([GLOW_W, GLOW_H], &bytes)
}

impl Glow {
    pub fn new(ctx: &egui::Context, cells: Vec<[f32; 3]>) -> Glow {
        let tex = ctx.load_texture("glow", glow_image(&cells), TextureOptions::LINEAR);
        Glow { tex, want: cells.clone(), now: cells }
    }

    pub fn paint(&mut self, ctx: &egui::Context, painter: &egui::Painter, dt: f32) {
        if self.now != self.want {
            let k = 1.0 - (-dt / 0.15).exp();
            for (a, b) in self.now.iter_mut().flatten().zip(self.want.iter().flatten()) {
                *a += (b - *a) * k;
                if (b - *a).abs() < 0.5 {
                    *a = *b;
                }
            }
            self.tex.set(glow_image(&self.now), TextureOptions::LINEAR);
            ctx.request_repaint();
        }
        painter.image(self.tex.id(), ctx.screen_rect(), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Color32) -> f64 {
        let f = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// A panel's colour as it ends up over the glow.
    fn over(panel: Color32, alpha: f32, back: [u8; 3]) -> Color32 {
        let m = |p: u8, b: u8| (p as f32 * alpha + b as f32 * (1.0 - alpha)).round() as u8;
        Color32::from_rgb(m(panel.r(), back[0]), m(panel.g(), back[1]), m(panel.b(), back[2]))
    }

    /// An endless fade would keep the window repainting for nothing.
    #[test]
    fn a_fade_ends_on_the_new_colours_and_stops() {
        let (a, b) = (Pal::of(&Colors::default()), Pal::of(&Colors { accent1: [255, 255, 0], tint: 0.06, ..Colors::default() }));
        let mut fade = Fade::new(a);
        assert!(fade.step(a, 0.016).is_none());
        let mut last = a;
        for _ in 0..(FADE_SECS / 0.016) as usize + 2 {
            if let Some(p) = fade.step(b, 0.016) {
                last = p;
            }
        }
        assert!(last == b && fade.step(b, 0.016).is_none());
    }

    #[test]
    fn readable_whatever_a_picture_brings() {
        let picture: [[u8; 3]; 13] = [
            [255, 255, 255], [0, 0, 0], [128, 128, 128], [222, 222, 222], [40, 20, 10], [255, 255, 0], [255, 0, 0],
            [0, 255, 0], [0, 0, 255], [0, 255, 255], [255, 0, 255], [160, 120, 255], [238, 132, 204],
        ];
        for accent in picture {
            for hue in [0.0, 60.0, 120.0, 180.0, 240.0, 300.0] {
                for tint in [0.0, 0.03, 0.06] {
                    for depth in [0.0, 0.5, 1.0] {
                        let c = Colors { accent1: accent, accent2: accent, hue, tint, depth, manual: true, ..Colors::default() };
                        let p = Pal::of(&c);
                        let case = format!("accent {accent:?} hue {hue} tint {tint} depth {depth}");
                        assert!(contrast(p.on_fill, p.fill) >= 4.5, "button text: {case}");
                        assert!(contrast(p.title, p.select) >= 4.5, "selected item: {case}");
                        assert!(contrast(p.text, p.select) >= 4.5, "selected row: {case}");
                        assert!(contrast(p.text, p.row_hover) >= 4.5, "row: {case}");
                        assert!(contrast(p.strong, p.hover) >= 4.5, "hovered button: {case}");
                        for lit in picture.map(glow_color) {
                            let side = over(p.side, SIDE_ALPHA, lit);
                            let stage = over(p.stage, STAGE_ALPHA, lit);
                            assert!(contrast(p.text, side) >= 4.5, "text on the side panel over {lit:?}: {case}");
                            assert!(contrast(p.weak, side) >= 4.5, "notes on the side panel over {lit:?}: {case}");
                            assert!(contrast(p.accent, over(p.top, TOP_ALPHA, lit)) >= 4.5, "the name on the top bar over {lit:?}: {case}");
                            assert!(contrast(p.weak, stage) >= 4.5, "the footer over {lit:?}: {case}");
                            assert!(contrast(p.accent, stage) >= 3.0, "links over {lit:?}: {case}");
                        }
                    }
                }
            }
        }
    }
}
