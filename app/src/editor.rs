//! The editor window: which pictures go into which of Live's panels and how they look, and the
//! interface colours, over a preview of Live drawn with the overlay's own shader. Changes are
//! saved as they happen; the background engine picks them up and shows them in Live.

use crate::config::{self, Colors, Config, Fit, Layer, Skin, Snapshot, APP_NAME, ZONES};
use crate::engine::scene;
use crate::palette::{self, pal, Fade, Glow, Pal};
use crate::preview::{self, theme_sig, Preview};
use crate::lang::{self, t};
use crate::update::{self, Release};
use crate::{live, log, theme};
use eframe::egui::{
    self, pos2, vec2, Align, Color32, ColorImage, CornerRadius, CursorIcon, Event, FontData, FontDefinitions,
    FontFamily, FontId, Id, Key, LayerId, Layout, Margin, Modifiers, MouseWheelUnit, Pos2, Rect, RichText, Sense,
    Slider, Stroke, StrokeKind, TextStyle, TextureHandle, TextureOptions, UiBuilder,
};
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, POINT};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetCursorPos, GetSystemMetrics, IsIconic, SetForegroundWindow, ShowWindow, SM_CXSCREEN, SM_CYSCREEN,
    SW_RESTORE,
};

const EXTS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "gif"];

/// `updated`: started by an update, as the old version's editor closes.
pub fn run(updated: bool) {
    // SKINLAB_SHOT=out.png: draw the window off-screen, save a picture of it and quit (no settings
    // are written, the engine is not touched); SKINLAB_LANG=en|ru picks the language for it
    let shot = std::env::var_os("SKINLAB_SHOT").map(PathBuf::from);
    let lang_env = std::env::var("SKINLAB_LANG").ok().filter(|_| shot.is_some());
    lang::set(&lang_env.unwrap_or_else(|| config::load::<Config>(&config::config_path()).lang));
    if shot.is_none() {
        if !first_instance(if updated { Duration::from_secs(15) } else { Duration::ZERO }) {
            return;
        }
        if !config::config_path().exists() {
            // the very first start: a skin to fill, and Skin Lab starts with Windows from now on
            let mut cfg = Config::default();
            cfg.skins.push(Skin::default());
            let _ = config::save(&config::config_path(), &cfg);
            if !in_build_dir() {
                config::set_autostart(true);
            }
        }
        if updated {
            // the old version's engine may still be on its way out
            config::stop_engine(Duration::from_secs(3));
        }
        if !config::engine_running() {
            config::start_engine();
        }
        // the old exe can go once nothing runs it any more
        std::thread::spawn(|| {
            for _ in 0..60 {
                if update::tidy() {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }
    let icon = egui::IconData { rgba: crate::icon::rgba(64), width: 64, height: 64 };
    let size = if shot.is_some() { [1600.0, 900.0] } else { window_size() };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_inner_size(size)
        .with_min_inner_size([980.0, 620.0])
        .with_icon(Arc::new(icon))
        .with_drag_and_drop(true);
    if shot.is_some() {
        viewport = viewport.with_position([-30000.0, -30000.0]).with_active(false).with_taskbar(false);
    }
    let options = eframe::NativeOptions { viewport, centered: shot.is_none(), ..Default::default() };
    if let Err(e) = eframe::run_native(APP_NAME, options, Box::new(move |cc| Ok(Box::new(Editor::new(cc, shot, updated))))) {
        log!("editor failed: {e}");
    }
}

/// One editor at a time: a second start brings the open one forward instead. Waits up to `wait`
/// for an editor that is closing.
fn first_instance(wait: Duration) -> bool {
    let until = Instant::now() + wait;
    unsafe {
        loop {
            match CreateMutexW(None, false, w!("Local\\SkinLabEditor")) {
                Ok(m) if GetLastError() == ERROR_ALREADY_EXISTS => {
                    let _ = CloseHandle(m);
                }
                // the mutex stays open as long as the editor runs
                _ => return true,
            }
            if Instant::now() >= until {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if let Ok(h) = FindWindowW(config::EDITOR_CLASS, config::EDITOR_TITLE) {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
        false
    }
}

/// Most of the main screen, in points: Live's window is wide, and so is its preview.
fn window_size() -> [f32; 2] {
    let (w, h, dpi) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN), GetDpiForSystem()) };
    let s = dpi.max(96) as f32 / 96.0;
    [(w as f32 / s * 0.82).clamp(980.0, 2300.0), (h as f32 / s * 0.8).clamp(620.0, 1150.0)]
}

/// A build in the project's `target` folder must not register itself to start with Windows.
fn in_build_dir() -> bool {
    std::env::current_exe().map(|p| p.components().any(|c| c.as_os_str() == "target")).unwrap_or(true)
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
    let mut ours = Vec::new();
    for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-symbol", "seguisym.ttf")] {
        if let Ok(bytes) = std::fs::read(dir.join(file)) {
            fonts.font_data.insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
            ours.push(name.to_string());
        }
    }
    // Segoe UI first, its symbols next, egui's own fonts (with the emoji) after them
    let list = fonts.families.entry(FontFamily::Proportional).or_default();
    for (i, name) in ours.into_iter().enumerate() {
        list.insert(i, name);
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        let mut v = egui::Visuals::dark();
        v.slider_trailing_fill = true;
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(8);
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = CornerRadius::same(6);
        }
        s.visuals = v;
        s.spacing.item_spacing = vec2(8.0, 7.0);
        s.spacing.button_padding = vec2(10.0, 4.0);
        s.spacing.slider_width = 170.0;
        s.spacing.interact_size.y = 24.0;
        s.spacing.combo_width = 200.0;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.5));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(19.0));
    });
}

/// A frame of Live for the preview.
struct Frame {
    /// `key`: the panel colour these pixels really show
    snap: Snapshot,
    /// BGRA rows
    px: Arc<Vec<u8>>,
    /// the colours of the skin theme it shows, if it is one of ours
    from: Option<Colors>,
    /// a screenshot that ships with Skin Lab: Live has never been seen here
    stand_in: bool,
    gen: u64,
    /// theme signature of the version given to the preview (0: as captured)
    uploaded: Option<u64>,
    /// the version asked of the repainting thread
    asked: Option<u64>,
    taken: Option<SystemTime>,
}

/// A frame to repaint into the skin's theme, on the worker thread.
struct Job {
    gen: u64,
    sig: u64,
    px: Arc<Vec<u8>>,
    from_key: [u8; 3],
    from: Option<Colors>,
    to: Colors,
    to_key: [u8; 3],
}

struct Done {
    gen: u64,
    sig: u64,
    px: Vec<u8>,
    key: [u8; 3],
}

/// Pictures are moved and resized in frame pixels.
#[derive(Clone, Copy)]
enum Drag {
    Move { layer: usize, start: Pos2, anchor: [f32; 2], denom: [f32; 2] },
    Resize { layer: usize, fixed: Pos2, left: bool, top: bool, aspect: f32, zoom: f32, width: f32 },
}

enum Dialog {
    New(String),
    Rename(String),
    Delete,
}

enum Act {
    Select(usize),
    Remove(usize),
    Swap(usize, usize),
    Hide(usize),
    Replace(usize),
    ColorsFrom(usize),
    Duplicate(usize),
}

struct Shot {
    path: PathBuf,
    asked: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Upd {
    Idle,
    /// `true`: asked for by a click (an automatic check stays quiet)
    Checking(bool),
    Downloading,
    Restarting,
}

enum UpdMsg {
    Checked(Result<Option<Release>, String>),
    /// the exe the release was installed as
    Installed(Result<PathBuf, String>),
    Restarted,
}

const AUTHOR_URL: &str = "https://www.youtube.com/@blueberry.4677";

/// Release notes as plain text in the interface language: the English part comes first, the
/// Russian one after a `<!-- ru -->` line.
fn notes_text(notes: &str) -> String {
    let text = match notes.split_once("<!-- ru -->") {
        Some((en, ru)) => if lang::english() { en } else { ru },
        None => notes,
    };
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !(l.trim().len() >= 3 && l.trim().chars().all(|c| c == '-')))
        .map(|l| {
            let l = l.trim_start_matches('#').trim_start().replace("**", "").replace('`', "");
            match l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                Some(rest) => format!("• {rest}"),
                None => l,
            }
        })
        .collect();
    lines.join("\n").trim().to_string()
}

struct Editor {
    cfg: Config,
    /// the selected picture: index into the active skin's layers
    sel: Option<usize>,
    /// the panel new pictures go to
    zone: String,
    pv: Result<Preview, String>,
    frame: Option<Frame>,
    gens: u64,
    skin_hash: u64,
    tex: Option<TextureHandle>,
    rendered: u64,
    /// where the preview was drawn (screen points) and its points per frame pixel
    stage: Option<(Rect, f32)>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    thumbs: HashMap<String, Option<TextureHandle>>,
    thumb_budget: u32,
    drag: Option<Drag>,
    dialog: Option<Dialog>,
    focus_dialog: bool,
    toast: Option<(String, Instant)>,
    /// hash of the settings as last saved (or read)
    saved: u64,
    saved_sig: u64,
    save_due: Option<Instant>,
    cfg_mtime: Option<SystemTime>,
    snap_mtime: Option<SystemTime>,
    next_poll: Instant,
    next_status: Instant,
    next_snapshot: Instant,
    snapshot_due: Option<Instant>,
    focused: bool,
    live_open: bool,
    engine_on: bool,
    autostart: bool,
    shot: Option<Shot>,
    frames: u64,
    /// the window's own colours on their way to the skin's
    fade: Fade,
    glow: Option<Glow>,
    /// a newer version, once a check found one
    release: Option<Release>,
    update_open: bool,
    update: Upd,
    update_error: Option<String>,
    update_tx: Sender<UpdMsg>,
    update_rx: Receiver<UpdMsg>,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn retheme_worker(rx: Receiver<Job>, tx: Sender<Done>, ctx: egui::Context) {
    while let Ok(mut job) = rx.recv() {
        // only the newest wish counts
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }
        let mut px = job.px.as_ref().clone();
        preview::retheme(&mut px, job.from_key, job.from.as_ref(), &job.to, job.to_key);
        if tx.send(Done { gen: job.gen, sig: job.sig, px, key: job.to_key }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// The name a person gave the picture: its file name without our "-1a2b3c4d" suffix.
fn picture_name(path: &str) -> String {
    let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match stem.rsplit_once('-') {
        Some((name, hash)) if hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()) && !name.is_empty() => name.to_string(),
        _ => stem,
    };
    name.replace('_', " ")
}

fn is_picture(p: &Path) -> bool {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| EXTS.contains(&e.as_str()))
}

fn pick_files() -> Vec<PathBuf> {
    rfd::FileDialog::new().set_title(t("Картинки для скина", "Pictures for the skin")).add_filter(t("Картинки", "Pictures"), &EXTS).pick_files().unwrap_or_default()
}

fn pick_file() -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(t("Другая картинка", "Another picture")).add_filter(t("Картинки", "Pictures"), &EXTS).pick_file()
}

fn inside(r: [f32; 4], p: Pos2) -> bool {
    p.x >= r[0] && p.y >= r[1] && p.x < r[0] + r[2] && p.y < r[1] + r[3]
}

/// Cover cannot leave a gap in its panel; the others may stick out a long way.
fn clamp_layer(l: &mut Layer) {
    if l.fit == Fit::Cover {
        l.zoom = l.zoom.clamp(1.0, 6.0);
        l.anchor = l.anchor.map(|a| a.clamp(0.0, 1.0));
    } else {
        l.zoom = l.zoom.clamp(0.05, 6.0);
        l.anchor = l.anchor.map(|a| a.clamp(-1.0, 2.0));
    }
}

/// The anchor that puts a picture at `pl` (x, y, w, h) in panel `r`.
fn anchor_for(r: [f32; 4], pl: [f32; 4], old: [f32; 2]) -> [f32; 2] {
    let a = |k: usize| {
        let d = r[k + 2] - pl[k + 2];
        if d.abs() > 0.5 {
            (pl[k] - r[k]) / d
        } else {
            old[k]
        }
    };
    [a(0), a(1)]
}

/// The part of a texture that fills `rect` without stretching.
fn cover_uv(size: [usize; 2], rect: Rect) -> Rect {
    let ta = size[0] as f32 / size[1].max(1) as f32;
    let ra = rect.width() / rect.height().max(1.0);
    if ta > ra {
        let w = ra / ta;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = ta / ra;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    }
}

fn section(ui: &mut egui::Ui, title: &str, note: Option<&str>) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).size(17.0).strong().color(pal().title));
    if let Some(n) = note {
        ui.label(RichText::new(n).small().color(pal().weak));
    }
    ui.add_space(4.0);
}

/// A main button, in the skin's accent. Its light rim on hover shows on a bright accent too.
fn primary(text: &str, height: f32) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let p = pal();
        let r = ui.add(egui::Button::new(RichText::new(text).strong().color(p.on_fill)).fill(p.fill).min_size(vec2(0.0, height)));
        if r.hovered() {
            ui.painter().rect_stroke(r.rect, 6.0, Stroke::new(1.5, p.title), StrokeKind::Outside);
        }
        r
    }
}

/// A strip going through `stops` from left to right.
fn strip(painter: &egui::Painter, r: Rect, stops: &[Color32]) {
    let mut m = egui::Mesh::default();
    for (i, c) in stops.iter().enumerate() {
        let x = r.left() + r.width() * i as f32 / (stops.len() - 1) as f32;
        m.colored_vertex(pos2(x, r.top()), *c);
        m.colored_vertex(pos2(x, r.bottom()), *c);
        if i > 0 {
            let n = i as u32 * 2;
            m.add_triangle(n - 2, n - 1, n);
            m.add_triangle(n - 1, n, n + 1);
        }
    }
    painter.add(m);
}

fn icon(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, tip: &str) -> egui::Response {
    ui.add(egui::Button::new(text).frame(false).min_size(vec2(24.0, 24.0))).on_hover_text(tip)
}

/// A slider showing `v` as percent; writes back only when moved.
fn pct_slider(ui: &mut egui::Ui, v: &mut f32, range: RangeInclusive<f32>, log: bool) -> bool {
    let mut x = *v * 100.0;
    let r = ui.add(Slider::new(&mut x, range).suffix(" %").fixed_decimals(0).logarithmic(log));
    if r.changed() {
        *v = x / 100.0;
        true
    } else {
        false
    }
}

fn zone_combo(ui: &mut egui::Ui, id: &str, zone: &mut String) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).selected_text(config::zone_name(zone)).width(200.0).show_ui(ui, |ui| {
        for z in ZONES {
            if ui.selectable_label(zone.as_str() == z, config::zone_name(z)).clicked() && zone.as_str() != z {
                *zone = z.to_string();
                changed = true;
            }
        }
    });
    changed
}

fn pill(painter: &egui::Painter, at: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(13.5), Color32::WHITE);
    let r = Rect::from_min_size(at, galley.size() + vec2(14.0, 8.0));
    painter.rect_filled(r, 7.0, Color32::from_black_alpha(190));
    painter.galley(r.min + vec2(7.0, 4.0), galley, Color32::WHITE);
}

impl Editor {
    fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>, updated: bool) -> Editor {
        setup_style(&cc.egui_ctx);
        let mut cfg: Config = config::load(&config::config_path());
        let saved = config::fnv(&serde_json::to_vec(&cfg).unwrap_or_default());
        if cfg.skins.is_empty() {
            cfg.skins.push(Skin::default());
        }
        cfg.active = cfg.active.min(cfg.skins.len() - 1);
        let colors = Pal::of(cfg.skins[cfg.active].wanted());
        palette::wear(&cc.egui_ctx, colors, shot.is_none());
        let (jobs, job_rx) = channel();
        let (done_tx, done) = channel();
        let (update_tx, update_rx) = channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || retheme_worker(job_rx, done_tx, ctx));
        let now = Instant::now();
        let mut e = Editor {
            cfg,
            sel: None,
            zone: "window".into(),
            pv: Preview::new(),
            frame: None,
            gens: 0,
            skin_hash: 0,
            tex: None,
            rendered: 0,
            stage: None,
            jobs,
            done,
            thumbs: HashMap::new(),
            thumb_budget: 0,
            drag: None,
            dialog: None,
            focus_dialog: false,
            toast: None,
            saved,
            saved_sig: 0,
            save_due: None,
            cfg_mtime: mtime(&config::config_path()),
            snap_mtime: None,
            next_poll: now,
            next_status: now,
            next_snapshot: now + Duration::from_secs(20),
            snapshot_due: None,
            focused: false,
            live_open: false,
            engine_on: false,
            autostart: false,
            shot: shot.map(|path| Shot { path, asked: false }),
            frames: 0,
            fade: Fade::new(colors),
            glow: None,
            release: None,
            update_open: false,
            update: Upd::Idle,
            update_error: None,
            update_tx,
            update_rx,
        };
        e.saved_sig = theme_sig(&e.skin().colors);
        e.load_frame();
        if let Some(i) = std::env::var("SKINLAB_SHOT_SEL").ok().and_then(|s| s.parse::<usize>().ok()) {
            e.select(i);
        }
        if e.shot.is_none() {
            e.ask_snapshot();
            if updated {
                e.toast(format!("{} {}", t("Ableton Skin Lab обновлён до версии", "Ableton Skin Lab is updated to version"), update::VERSION));
            }
            if !in_build_dir() {
                e.check_update(false);
            }
        }
        e
    }

    fn skin(&self) -> &Skin {
        &self.cfg.skins[self.cfg.active]
    }

    fn skin_mut(&mut self) -> &mut Skin {
        let i = self.cfg.active;
        &mut self.cfg.skins[i]
    }

    fn cfg_hash(&self) -> u64 {
        config::fnv(&serde_json::to_vec(&self.cfg).unwrap_or_default())
    }

    fn toast(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        log!("{m}");
        self.toast = Some((m, Instant::now()));
    }

    fn select(&mut self, i: usize) {
        let zone = self.skin().layers.get(i).map(|l| l.zone.clone());
        if let Some(z) = zone {
            self.zone = z;
            self.sel = Some(i);
        }
    }

    // ---- the frame of Live under the preview ----

    fn load_frame(&mut self) {
        self.snap_mtime = mtime(&config::snapshot_paths().0);
        let (snap, px, stand_in) = match preview::load_snapshot() {
            Some((snap, px)) => (snap, px, false),
            None if self.frame.is_none() => {
                let (snap, px) = preview::stand_in();
                (snap, px, true)
            }
            None => return,
        };
        self.gens += 1;
        self.frame = Some(Frame {
            from: snap.colors.clone().filter(|_| snap.theme_ok),
            snap,
            px: Arc::new(px),
            stand_in,
            gen: self.gens,
            uploaded: None,
            asked: None,
            taken: if stand_in { None } else { self.snap_mtime },
        });
    }

    /// Hands the skin and a frame in its theme to the preview: the captured frame if Live
    /// already shows this theme, otherwise a repainted copy (made on the worker thread; the
    /// frame as captured stands in meanwhile).
    fn sync_preview(&mut self) {
        let Ok(pv) = &mut self.pv else { return };
        let skin = &self.cfg.skins[self.cfg.active];
        let h = config::fnv(&serde_json::to_vec(skin).unwrap_or_default());
        if h != self.skin_hash {
            // the preview shows the colours being tried
            let mut shown = skin.clone();
            if let Some(d) = shown.draft.take() {
                shown.colors = d;
            }
            pv.set_skin(&shown);
            self.skin_hash = h;
        }
        let to_key = pv.skin_key();
        let sig = theme_sig(skin.wanted());
        let Some(f) = &mut self.frame else { return };
        let want = if to_key.is_some() { sig } else { 0 };
        if f.uploaded == Some(want) {
            return;
        }
        let in_theme = want == 0 || (f.from.as_ref().map(theme_sig) == Some(sig) && Some(f.snap.key) == to_key);
        if in_theme || f.uploaded.is_none() {
            match pv.set_frame(f.snap.clone(), &f.px) {
                Ok(()) => f.uploaded = Some(if in_theme { want } else { 0 }),
                Err(e) => {
                    log!("preview frame: {e}");
                    return;
                }
            }
        }
        if let (false, Some(to_key)) = (in_theme, to_key) {
            if f.asked != Some(want) {
                let job = Job { gen: f.gen, sig: want, px: f.px.clone(), from_key: f.snap.key, from: f.from.clone(), to: skin.wanted().clone(), to_key };
                let _ = self.jobs.send(job);
                f.asked = Some(want);
            }
        }
    }

    fn receive_done(&mut self) {
        while let Ok(d) = self.done.try_recv() {
            let (Ok(pv), Some(f)) = (&mut self.pv, &mut self.frame) else { continue };
            if d.gen != f.gen {
                continue;
            }
            let snap = Snapshot { key: d.key, ..f.snap.clone() };
            if pv.set_frame(snap, &d.px).is_ok() {
                f.uploaded = Some(d.sig);
            }
        }
    }

    /// Renders the preview `width_px` physical pixels wide, if anything changed.
    fn render_preview(&mut self, ctx: &egui::Context, width_px: f32) {
        let Ok(pv) = &mut self.pv else { return };
        let (Some(fs), Some(f)) = (pv.frame_size(), &self.frame) else { return };
        let scale = (fs[0] as f32 / width_px.max(64.0)).max(1.0);
        let key = config::fnv(
            &[self.skin_hash, f.gen, f.uploaded.unwrap_or(0), scale.to_bits() as u64].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(),
        );
        if key == self.rendered && self.tex.is_some() {
            return;
        }
        self.rendered = key;
        match pv.render(scale) {
            Ok(img) => {
                let ci = ColorImage::from_rgba_premultiplied([img.width() as usize, img.height() as usize], img.as_raw());
                match &mut self.tex {
                    Some(t) => t.set(ci, TextureOptions::LINEAR),
                    None => self.tex = Some(ctx.load_texture("preview", ci, TextureOptions::LINEAR)),
                }
                let cells = palette::glow_of(&img);
                match &mut self.glow {
                    Some(g) => g.want = cells,
                    None => self.glow = Some(Glow::new(ctx, cells)),
                }
            }
            Err(e) => log!("preview: {e}"),
        }
    }

    fn zone_rect(&self, zone: &str) -> Option<[f32; 4]> {
        let f = self.frame.as_ref()?;
        if zone == "window" {
            return Some([0.0, 0.0, f.snap.size[0] as f32, f.snap.size[1] as f32]);
        }
        f.snap.zones.get(zone).map(|r| r.map(|v| v as f32)).filter(|r| r[2] > 0.0 && r[3] > 0.0)
    }

    /// The smallest panel under a frame point.
    fn zone_at(&self, p: Pos2) -> Option<String> {
        let f = self.frame.as_ref()?;
        f.snap
            .zones
            .iter()
            .filter(|(_, r)| inside(r.map(|v| v as f32), p))
            .min_by_key(|(_, r)| r[2] as i64 * r[3] as i64)
            .map(|(id, _)| id.clone())
    }

    /// Where picture `i` is drawn and its panel, in frame pixels.
    fn placed(&self, i: usize) -> Option<([f32; 4], [f32; 4])> {
        let l = self.skin().layers.get(i)?;
        let r = self.zone_rect(&l.zone)?;
        let (pw, ph) = self.pv.as_ref().ok()?.pic_size(&l.picture)?;
        Some((scene::place(r, l, pw, ph), r))
    }

    /// The topmost visible picture at a frame point.
    fn layer_at(&self, p: Pos2) -> Option<usize> {
        let layers = &self.skin().layers;
        (0..layers.len()).rev().filter(|&i| !layers[i].hidden).find(|&i| self.placed(i).is_some_and(|(pl, r)| inside(pl, p) && inside(r, p)))
    }

    // ---- editing ----

    fn add_picture(&mut self, src: &Path, zone: &str) {
        if let Err(e) = self.try_add_picture(src, zone) {
            self.toast(format!("{} {}: {e}", t("Не получилось добавить", "Could not add"), src.display()));
        }
    }

    fn try_add_picture(&mut self, src: &Path, zone: &str) -> Result<(), String> {
        let pic = config::import_picture(src)?;
        let img = theme::open_image(Path::new(&pic))?;
        let aspect = self.zone_rect(zone).map(|r| r[2] / r[3].max(1.0)).unwrap_or(2.5);
        let mut layer = theme::suggest_layer(&img, aspect);
        layer.picture = pic;
        layer.zone = zone.to_string();
        let name = picture_name(&src.to_string_lossy());
        let skin = self.skin_mut();
        if layer.fit != Fit::Free && skin.layers.iter().any(|l| l.zone == zone && l.fit != Fit::Free && !l.hidden) {
            // the panel already has a background picture: this one goes on top as a sticker
            layer.fit = Fit::Free;
            layer.zoom = 0.75;
            layer.anchor = [0.92, 0.95];
            layer.soft = layer.soft.max(0.12);
        }
        let auto = !skin.wanted().manual && (skin.layers.is_empty() || zone == "window");
        skin.layers.push(layer);
        let i = skin.layers.len() - 1;
        if auto {
            self.set_draft(theme::colors_from_image(&img, Some(name)));
        }
        self.select(i);
        Ok(())
    }

    /// New interface colours wait in the draft until they are applied: a new theme brings Live
    /// to the front.
    fn set_draft(&mut self, c: Colors) {
        let s = self.skin_mut();
        s.draft = (c != s.colors).then_some(c);
    }

    fn apply_colors(&mut self) {
        let s = self.skin_mut();
        if let Some(d) = s.draft.take() {
            s.colors = d;
        }
    }

    fn remove_layer(&mut self, i: usize) {
        let skin = self.skin_mut();
        if i < skin.layers.len() {
            skin.layers.remove(i);
        }
        self.sel = match self.sel {
            Some(s) if s == i => None,
            Some(s) if s > i => Some(s - 1),
            s => s,
        };
        self.drag = None;
    }

    fn duplicate_layer(&mut self, i: usize) {
        let skin = self.skin_mut();
        let Some(mut l) = skin.layers.get(i).cloned() else { return };
        if l.fit == Fit::Free {
            // a little aside, so that the copy shows
            l.anchor[0] = (l.anchor[0] - 0.1).max(-1.0);
        }
        skin.layers.insert(i + 1, l);
        self.select(i + 1);
    }

    fn swap_layers(&mut self, a: usize, b: usize) {
        let skin = self.skin_mut();
        if a < skin.layers.len() && b < skin.layers.len() {
            skin.layers.swap(a, b);
            if self.sel == Some(a) {
                self.sel = Some(b);
            } else if self.sel == Some(b) {
                self.sel = Some(a);
            }
        }
    }

    fn colors_from(&mut self, path: &str) {
        match theme::open_image(Path::new(path)) {
            Ok(img) => {
                let name = picture_name(path);
                self.set_draft(theme::colors_from_image(&img, Some(name)));
            }
            Err(e) => self.toast(format!("{}: {e}", t("Не открылась картинка", "The picture did not open"))),
        }
    }

    fn apply(&mut self, a: Act) {
        match a {
            Act::Select(i) => {
                if self.sel == Some(i) {
                    self.sel = None;
                } else {
                    self.select(i);
                }
            }
            Act::Remove(i) => self.remove_layer(i),
            Act::Swap(a, b) => self.swap_layers(a, b),
            Act::Hide(i) => {
                if let Some(l) = self.skin_mut().layers.get_mut(i) {
                    l.hidden = !l.hidden;
                }
            }
            Act::Replace(i) => {
                if let Some(f) = pick_file() {
                    match config::import_picture(&f) {
                        Ok(p) => {
                            if let Some(l) = self.skin_mut().layers.get_mut(i) {
                                l.picture = p;
                            }
                        }
                        Err(e) => self.toast(e),
                    }
                }
            }
            Act::ColorsFrom(i) => {
                if let Some(p) = self.skin().layers.get(i).map(|l| l.picture.clone()) {
                    self.colors_from(&p);
                }
            }
            Act::Duplicate(i) => self.duplicate_layer(i),
        }
    }

    fn start_drag(&mut self, p: Pos2, corner: Option<usize>) {
        if let (Some(i), Some(c)) = (self.sel, corner) {
            if let Some((pl, _)) = self.placed(i) {
                let (left, top) = (c % 2 == 0, c < 2);
                let fixed = pos2(if left { pl[0] + pl[2] } else { pl[0] }, if top { pl[1] + pl[3] } else { pl[1] });
                let zoom = self.skin().layers[i].zoom;
                self.drag = Some(Drag::Resize { layer: i, fixed, left, top, aspect: pl[2] / pl[3].max(1e-3), zoom, width: pl[2] });
                return;
            }
        }
        let Some(i) = self.layer_at(p) else { return };
        self.select(i);
        let Some((pl, r)) = self.placed(i) else { return };
        let anchor = self.skin().layers[i].anchor;
        self.drag = Some(Drag::Move { layer: i, start: p, anchor, denom: [r[2] - pl[2], r[3] - pl[3]] });
    }

    fn update_drag(&mut self, p: Pos2) {
        let Some(drag) = self.drag else { return };
        match drag {
            Drag::Move { layer, start, anchor, denom } => {
                let d = [p.x - start.x, p.y - start.y];
                let Some(l) = self.skin_mut().layers.get_mut(layer) else { return };
                for k in 0..2 {
                    if denom[k].abs() > 0.5 {
                        l.anchor[k] = anchor[k] + d[k] / denom[k];
                    }
                }
                clamp_layer(l);
            }
            Drag::Resize { layer, fixed, left, top, aspect, zoom, width } => {
                let Some(r) = self.skin().layers.get(layer).and_then(|l| self.zone_rect(&l.zone)) else { return };
                let w = (p.x - fixed.x).abs().max((p.y - fixed.y).abs() * aspect).max(12.0);
                let h = w / aspect;
                let x = if left { fixed.x - w } else { fixed.x };
                let y = if top { fixed.y - h } else { fixed.y };
                let Some(l) = self.skin_mut().layers.get_mut(layer) else { return };
                l.zoom = zoom * w / width;
                l.anchor = anchor_for(r, [x, y, w, h], l.anchor);
                clamp_layer(l);
            }
        }
    }

    /// Scales picture `i` by `factor` around the frame point `p`.
    fn zoom_at(&mut self, i: usize, p: Pos2, factor: f32) {
        let Some((pl, r)) = self.placed(i) else { return };
        let Some(l) = self.skin_mut().layers.get_mut(i) else { return };
        let old = l.zoom;
        l.zoom *= factor;
        clamp_layer(l);
        let f = l.zoom / old;
        let (w, h) = (pl[2] * f, pl[3] * f);
        let u = [(p.x - pl[0]) / pl[2], (p.y - pl[1]) / pl[3]];
        l.anchor = anchor_for(r, [p.x - u[0] * w, p.y - u[1] * h, w, h], l.anchor);
        clamp_layer(l);
    }

    fn nudge(&mut self, i: usize, d: [f32; 2]) {
        let Some((pl, r)) = self.placed(i) else { return };
        let Some(l) = self.skin_mut().layers.get_mut(i) else { return };
        l.anchor = anchor_for(r, [pl[0] + d[0], pl[1] + d[1], pl[2], pl[3]], l.anchor);
        clamp_layer(l);
    }

    fn unique_name(&self, base: &str) -> String {
        let base = base.trim();
        let base = if base.is_empty() { t("Скин", "Skin") } else { base };
        let taken = |n: &str| self.cfg.skins.iter().any(|s| s.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base} {n}")).find(|n| !taken(n)).unwrap_or_default()
    }

    fn copy_skin(&mut self) {
        let mut s = self.skin().clone();
        s.name = self.unique_name(&format!("{} {}", s.name, t("(копия)", "(copy)")));
        self.cfg.skins.insert(self.cfg.active + 1, s);
        self.cfg.active += 1;
        self.sel = None;
    }

    fn thumb(&mut self, ctx: &egui::Context, path: &str) -> Option<TextureHandle> {
        if let Some(t) = self.thumbs.get(path) {
            return t.clone();
        }
        // a few per frame, so that a long list does not freeze the window
        if self.thumb_budget == 0 {
            ctx.request_repaint();
            return None;
        }
        self.thumb_budget -= 1;
        let t = theme::open_image(Path::new(path)).ok().map(|img| {
            let small = img.thumbnail(160, 160).to_rgba8();
            let ci = ColorImage::from_rgba_unmultiplied([small.width() as usize, small.height() as usize], small.as_raw());
            ctx.load_texture(format!("thumb:{path}"), ci, TextureOptions::LINEAR)
        });
        self.thumbs.insert(path.to_string(), t.clone());
        t
    }

    // ---- outside world ----

    fn ask_snapshot(&mut self) {
        let now = Instant::now();
        self.next_snapshot = now + Duration::from_secs(20);
        // the engine may still be starting
        self.snapshot_due = if config::request_snapshot() { None } else { Some(now + Duration::from_secs(3)) };
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(i.focused));
        if self.shot.is_none() && focused && !self.focused {
            self.ask_snapshot();
        }
        self.focused = focused;
        if now < self.next_poll {
            return;
        }
        self.next_poll = now + Duration::from_millis(700);
        // settings changed elsewhere (the tray menu)
        let m = mtime(&config::config_path());
        if m != self.cfg_mtime && self.save_due.is_none() {
            self.cfg_mtime = m;
            let mut cfg: Config = config::load(&config::config_path());
            if !cfg.skins.is_empty() && cfg != self.cfg {
                cfg.active = cfg.active.min(cfg.skins.len() - 1);
                if cfg.active != self.cfg.active {
                    self.sel = None;
                }
                self.cfg = cfg;
                self.saved = self.cfg_hash();
                self.saved_sig = theme_sig(&self.skin().colors);
                if self.sel.is_some_and(|i| i >= self.skin().layers.len()) {
                    self.sel = None;
                }
            }
        }
        if mtime(&config::snapshot_paths().0) != self.snap_mtime {
            self.load_frame();
        }
        if now >= self.next_status {
            self.next_status = now + Duration::from_secs(2);
            self.engine_on = config::engine_running();
            self.live_open = live::find().is_some();
            self.autostart = config::autostart();
        }
        if self.shot.is_none() && ((focused && now >= self.next_snapshot) || self.snapshot_due.is_some_and(|t| now >= t)) {
            self.ask_snapshot();
        }
    }

    /// `now_please`: right away (the window is closing)
    fn save_if_changed(&mut self, ctx: &egui::Context, now_please: bool) {
        if self.shot.is_some() {
            return;
        }
        let h = self.cfg_hash();
        if h == self.saved {
            self.save_due = None;
            return;
        }
        // at most four times a second while something is being dragged
        let now = Instant::now();
        let due = *self.save_due.get_or_insert(now + Duration::from_millis(250));
        if now < due && !now_please {
            ctx.request_repaint_after(due - now);
            return;
        }
        self.save_due = None;
        match config::save(&config::config_path(), &self.cfg) {
            Ok(()) => {
                self.saved = h;
                self.cfg_mtime = mtime(&config::config_path());
                config::notify_engine();
                // once Live shows the new theme, a fresh look at it
                let sig = theme_sig(&self.skin().colors);
                if sig != self.saved_sig {
                    self.saved_sig = sig;
                    self.snapshot_due = Some(now + Duration::from_millis(4500));
                }
            }
            Err(e) => self.toast(format!("{}: {e}", t("Не сохранилось", "Could not save"))),
        }
    }

    // ---- updates ----

    fn check_update(&mut self, asked: bool) {
        match self.update {
            Upd::Idle => {
                self.update = Upd::Checking(asked);
                let tx = self.update_tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(UpdMsg::Checked(update::check()));
                });
            }
            // the check at start is still running: its answer is shown now
            Upd::Checking(_) if asked => self.update = Upd::Checking(true),
            _ => {}
        }
    }

    fn install_update(&mut self, r: Release) {
        self.update = Upd::Downloading;
        self.update_error = None;
        let tx = self.update_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(UpdMsg::Installed(update::install(&r)));
        });
    }

    fn receive_update(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.update_rx.try_recv() {
            match m {
                UpdMsg::Checked(found) => {
                    let asked = self.update == Upd::Checking(true);
                    self.update = Upd::Idle;
                    match found {
                        Ok(Some(r)) => {
                            log!("update {} is out", r.version);
                            self.release = Some(r);
                            self.update_open |= asked;
                        }
                        Ok(None) if asked => {
                            self.toast(format!("{} {}", t("Это последняя версия:", "This is the newest version:"), update::VERSION));
                        }
                        Ok(None) => {}
                        Err(e) if asked => self.toast(format!("{}: {e}", t("Не получилось проверить обновления", "Could not check for updates"))),
                        Err(e) => log!("update check: {e}"),
                    }
                }
                UpdMsg::Installed(Ok(exe)) => {
                    // the new version reads the settings as they are now
                    self.save_if_changed(ctx, true);
                    self.update = Upd::Restarting;
                    let tx = self.update_tx.clone();
                    std::thread::spawn(move || {
                        update::restart(&exe, true);
                        let _ = tx.send(UpdMsg::Restarted);
                    });
                }
                UpdMsg::Installed(Err(e)) => {
                    log!("update failed: {e}");
                    self.update = Upd::Idle;
                    self.update_error = Some(e);
                }
                UpdMsg::Restarted => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }

    /// The mouse in window points (also while files are dragged over the window, when the
    /// window gets no mouse events).
    fn cursor_in_window(&self, ctx: &egui::Context) -> Option<Pos2> {
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt).ok()? };
        let inner = ctx.input(|i| i.viewport().inner_rect)?;
        let ppp = ctx.pixels_per_point();
        Some(pos2(pt.x as f32 / ppp - inner.min.x, pt.y as f32 / ppp - inner.min.y))
    }

    fn take_dropped(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if files.is_empty() {
            return;
        }
        let at = self.cursor_in_window(ctx).and_then(|p| {
            let (img, k) = self.stage?;
            img.contains(p).then(|| pos2((p.x - img.min.x) / k, (p.y - img.min.y) / k))
        });
        let zone = at.and_then(|q| self.zone_at(q)).unwrap_or_else(|| self.zone.clone());
        let pictures: Vec<&PathBuf> = files.iter().filter(|f| is_picture(f)).collect();
        if pictures.is_empty() {
            self.toast(t("Это не картинка: подойдут PNG, JPG, WebP, BMP и GIF", "Not a picture: PNG, JPG, WebP, BMP and GIF will do"));
        }
        for f in pictures {
            self.add_picture(f, &zone);
        }
    }

    // ---- the window ----

    /// The window's colours follow the skin's, and so what the skin is trying out.
    fn follow_colors(&mut self, ctx: &egui::Context) {
        if let Some(p) = self.fade.step(Pal::of(self.skin().wanted()), self.fade_dt(ctx)) {
            palette::wear(ctx, p, self.shot.is_none());
            ctx.request_repaint();
        }
    }

    /// A picture of the window shows where a change ends, not its first frames.
    fn fade_dt(&self, ctx: &egui::Context) -> f32 {
        if self.shot.is_some() {
            10.0
        } else {
            ctx.input(|i| i.stable_dt)
        }
    }

    fn backdrop(&mut self, ctx: &egui::Context) {
        let painter = ctx.layer_painter(LayerId::background());
        painter.rect_filled(ctx.screen_rect(), 0.0, pal().stage);
        let dt = self.fade_dt(ctx);
        if let Some(g) = &mut self.glow {
            g.paint(ctx, &painter, dt);
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let p = pal();
        let bar = egui::TopBottomPanel::top("top").frame(egui::Frame::new().fill(p.top.gamma_multiply(palette::TOP_ALPHA)).inner_margin(Margin::symmetric(16, 10))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("✦").size(19.0).strong().color(p.accent2));
                ui.label(RichText::new(APP_NAME).size(19.0).strong().color(p.accent));
                if let Some(r) = &self.release {
                    let text = format!("⬆ {} {}", t("Обновление", "Update"), r.version);
                    let tip = t("Вышла новая версия Ableton Skin Lab", "A new version of Ableton Skin Lab is out");
                    if ui.add(primary(&text, 0.0)).on_hover_text(tip).clicked() {
                        self.update_open = true;
                    }
                }
                ui.add_space(18.0);
                let mut active = self.cfg.active;
                egui::ComboBox::from_id_salt("skin")
                    .selected_text(RichText::new(self.skin().name.as_str()).strong())
                    .width(230.0)
                    .show_ui(ui, |ui| {
                        for (i, s) in self.cfg.skins.iter().enumerate() {
                            ui.selectable_value(&mut active, i, s.name.as_str());
                        }
                    });
                if active != self.cfg.active {
                    self.cfg.active = active;
                    self.sel = None;
                    self.drag = None;
                }
                if ui.button(t("+ Новый", "+ New")).on_hover_text(t("Пустой скин", "An empty skin")).clicked() {
                    self.dialog = Some(Dialog::New(String::new()));
                    self.focus_dialog = true;
                }
                if ui.button(t("Копия", "Copy")).on_hover_text(t("Копия этого скина — попробовать вариант", "A copy of this skin, to try a variation")).clicked() {
                    self.copy_skin();
                }
                if ui.button(t("Переименовать", "Rename")).clicked() {
                    self.dialog = Some(Dialog::Rename(self.skin().name.clone()));
                    self.focus_dialog = true;
                }
                if ui.add_enabled(self.cfg.skins.len() > 1, egui::Button::new(t("Удалить", "Delete"))).clicked() {
                    self.dialog = Some(Dialog::Delete);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // right to left: EN, then RU
                    let en = lang::english();
                    for (code, name, on) in [("en", "EN", en), ("ru", "RU", !en)] {
                        if ui.selectable_label(on, name).on_hover_text(t("Язык интерфейса", "Interface language")).clicked() && !on {
                            self.cfg.lang = code.into();
                            lang::set(code);
                        }
                    }
                    ui.add_space(14.0);
                    let (text, color) = if self.live_open {
                        (t("● Live открыт", "● Live is open"), Color32::from_rgb(110, 220, 140))
                    } else {
                        (t("○ Live не запущен", "○ Live is not running"), p.weak)
                    };
                    ui.label(RichText::new(text).color(color));
                    if !self.engine_on
                        && ui
                            .button(t("Запустить фон", "Start background"))
                            .on_hover_text(t("Фоновая часть Ableton Skin Lab рисует картинки поверх Live", "The background part of Ableton Skin Lab draws the pictures over Live"))
                            .clicked()
                    {
                        config::start_engine();
                        self.next_status = Instant::now() + Duration::from_millis(1500);
                    }
                    ui.add_space(16.0);
                    let mut auto = self.autostart;
                    if ui.checkbox(&mut auto, t("Запускать с Windows", "Start with Windows")).changed() {
                        config::set_autostart(auto);
                        self.autostart = config::autostart();
                    }
                    ui.checkbox(&mut self.cfg.enabled, t("Скин включён", "Skin on"))
                        .on_hover_text(t("Выключи — и Live вернётся к своей обычной теме", "Turn it off and Live goes back to its usual theme"));
                });
            });
        });
        let edge = Rect::from_min_max(pos2(bar.response.rect.left(), bar.response.rect.bottom() - 2.0), bar.response.rect.right_bottom());
        strip(&ctx.layer_painter(LayerId::background()), edge, &[p.accent, p.accent2, Color32::TRANSPARENT]);
    }

    fn side_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("side")
            .resizable(true)
            .default_width(400.0)
            .width_range(340.0..=560.0)
            .frame(egui::Frame::new().fill(pal().side.gamma_multiply(palette::SIDE_ALPHA)).inner_margin(Margin::symmetric(14, 10)))
            .show(ctx, |ui| {
                egui::TopBottomPanel::bottom("apply")
                    .frame(egui::Frame::new().inner_margin(Margin { left: 0, right: 0, top: 10, bottom: 2 }))
                    .show_inside(ui, |ui| self.apply_bar(ui));
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.layers_ui(ui);
                    if let Some(i) = self.sel {
                        ui.add_space(8.0);
                        ui.separator();
                        self.layer_ui(ui, i);
                    }
                    ui.add_space(8.0);
                    ui.separator();
                    self.colors_ui(ui);
                    ui.add_space(12.0);
                });
            });
    }

    /// Live takes new interface colours only from here: a new theme brings Live to the front.
    fn apply_bar(&mut self, ui: &mut egui::Ui) {
        let pending = self.skin().draft.is_some();
        ui.horizontal(|ui| {
            let text = t("Применить в Live", "Apply to Live");
            if pending {
                if ui.add(primary(text, 32.0)).clicked() {
                    self.apply_colors();
                }
            } else {
                ui.add_enabled(false, egui::Button::new(RichText::new(text).strong()).fill(pal().row).min_size(vec2(0.0, 32.0)));
            }
            if pending
                && ui
                    .add(egui::Button::new(t("Отменить", "Revert")).min_size(vec2(0.0, 32.0)))
                    .on_hover_text(t("Вернуть цвета, которые сейчас в Live", "Back to the colours Live has now"))
                    .clicked()
            {
                self.skin_mut().draft = None;
            }
        });
        let note = if pending {
            t(
                "Новые цвета пока только в превью. Live перекрасится по кнопке — на миг выйдет вперёд со своими настройками.",
                "The new colours are only in the preview so far. Live repaints when you apply them, coming to the front with its settings for a moment.",
            )
        } else {
            t("В Live сейчас эти цвета. Картинки появляются в Live сразу.", "Live has these colours. Pictures show up in Live right away.")
        };
        ui.label(RichText::new(note).small().color(pal().weak));
    }

    fn layers_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, t("Картинки", "Pictures"), Some(t("Выше в списке — поверх остальных", "Higher in the list is on top")));
        let rows: Vec<(String, String, &'static str, bool, bool)> = self
            .skin()
            .layers
            .iter()
            .map(|l| {
                let failed = self.pv.as_ref().is_ok_and(|p| p.failed(&l.picture));
                (l.picture.clone(), picture_name(&l.picture), config::zone_name(&l.zone), l.hidden, failed)
            })
            .collect();
        if rows.is_empty() {
            ui.label(
                RichText::new(t(
                    "Пока пусто. Добавь картинку кнопкой ниже, перетащи файл прямо в окно или дважды щёлкни по панели на превью.",
                    "Nothing here yet. Add a picture with the button below, drop a file into the window, or double-click a panel in the preview.",
                ))
                    .color(pal().weak),
            );
            ui.add_space(4.0);
        }
        let n = rows.len();
        let mut act = None;
        for i in (0..n).rev() {
            let (path, name, zone, hidden, failed) = &rows[i];
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click());
            let p = pal();
            let selected = self.sel == Some(i);
            let fill = if selected {
                p.select
            } else if resp.hovered() {
                p.row_hover
            } else {
                p.row
            };
            ui.painter().rect_filled(rect, 8.0, fill);
            if resp.clicked() {
                act = Some(Act::Select(i));
            }
            let thumb = self.thumb(ui.ctx(), path);
            let mut row = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(6.0, 4.0))).layout(Layout::right_to_left(Align::Center)));
            row.spacing_mut().item_spacing.x = 2.0;
            if icon(&mut row, "✕", t("Убрать из скина", "Remove from the skin")).clicked() {
                act = Some(Act::Remove(i));
            }
            if icon(&mut row, "▼", t("Ниже", "Down")).clicked() && i > 0 {
                act = Some(Act::Swap(i, i - 1));
            }
            if icon(&mut row, "▲", t("Выше", "Up")).clicked() && i + 1 < n {
                act = Some(Act::Swap(i, i + 1));
            }
            let eye = RichText::new("👁").color(if *hidden { Color32::from_gray(90) } else { Color32::WHITE });
            if icon(&mut row, eye, if *hidden { t("Показать", "Show") } else { t("Спрятать", "Hide") }).clicked() {
                act = Some(Act::Hide(i));
            }
            row.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let (tr, _) = ui.allocate_exact_size(vec2(64.0, 42.0), Sense::hover());
                match &thumb {
                    Some(t) => {
                        let tint = if *hidden { Color32::from_gray(80) } else { Color32::WHITE };
                        ui.painter().image(t.id(), tr, cover_uv(t.size(), tr), tint);
                    }
                    None => {
                        ui.painter().rect_filled(tr, 4.0, Color32::from_gray(50));
                    }
                }
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let mut title = RichText::new(name.as_str());
                    if *hidden {
                        title = title.color(if selected { p.text } else { p.weak }).strikethrough();
                    }
                    ui.add(egui::Label::new(title).truncate());
                    let sub = if *failed {
                        RichText::new(t("не открылась", "did not open")).small().color(Color32::from_rgb(255, 120, 120))
                    } else {
                        RichText::new(*zone).small().color(if selected { p.text } else { p.weak })
                    };
                    ui.add(egui::Label::new(sub).truncate());
                });
            });
            ui.add_space(3.0);
        }
        if let Some(a) = act {
            self.apply(a);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(primary(t("+ Добавить картинку", "+ Add picture"), 30.0)).clicked() {
                let zone = self.zone.clone();
                for f in pick_files() {
                    self.add_picture(&f, &zone);
                }
            }
            ui.label(t("в", "to"));
            zone_combo(ui, "add_zone", &mut self.zone);
        });
    }

    fn layer_ui(&mut self, ui: &mut egui::Ui, i: usize) {
        if i >= self.skin().layers.len() {
            self.sel = None;
            return;
        }
        section(ui, t("Картинка", "Picture"), None);
        let has_window_bg = self.skin().layers.iter().any(|l| l.zone == "window" && l.fit != Fit::Free && !l.hidden);
        let mut act = None;
        let mut new_zone = None;
        let skin = self.skin_mut();
        let l = &mut skin.layers[i];
        egui::Grid::new("layer").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(t("Где", "Where"));
            if zone_combo(ui, "layer_zone", &mut l.zone) {
                new_zone = Some(l.zone.clone());
            }
            ui.end_row();

            ui.label(t("Как", "Fit"));
            ui.horizontal(|ui| {
                for (fit, name, tip) in [
                    (Fit::Cover, t("Заполнить", "Fill"), t("Картинка закрывает всю панель, лишнее обрезается", "The picture covers the whole panel; what sticks out is cut off")),
                    (Fit::Contain, t("Целиком", "Whole"), t("Видна вся картинка, по бокам — её размытые края", "The whole picture shows; its blurred edges fill the sides")),
                    (Fit::Free, t("Наклейка", "Sticker"), t("Просто картинка: двигай и меняй размер как хочешь", "Just the picture: move and resize it as you like")),
                ] {
                    if ui.selectable_label(l.fit == fit, name).on_hover_text(tip).clicked() && l.fit != fit {
                        l.fit = fit;
                        if fit == Fit::Free {
                            l.zoom = l.zoom.min(0.8);
                        }
                        clamp_layer(l);
                    }
                }
            });
            ui.end_row();

            ui.label(t("Размер", "Size"));
            let lo = if l.fit == Fit::Cover { 100.0 } else { 5.0 };
            if pct_slider(ui, &mut l.zoom, lo..=600.0, true) {
                clamp_layer(l);
            }
            ui.end_row();

            let (lo, hi) = if l.fit == Fit::Cover { (0.0, 100.0) } else { (-100.0, 200.0) };
            ui.label(t("Сдвиг ↔", "Shift ↔"));
            pct_slider(ui, &mut l.anchor[0], lo..=hi, false);
            ui.end_row();
            ui.label(t("Сдвиг ↕", "Shift ↕"));
            pct_slider(ui, &mut l.anchor[1], lo..=hi, false);
            ui.end_row();

            ui.label(t("Поворот", "Rotate"));
            ui.horizontal(|ui| {
                if ui.button("↻ 90°").clicked() {
                    l.rotate = (l.rotate + 1) % 4;
                }
                ui.toggle_value(&mut l.mirror, t("⇋ Зеркально", "⇋ Mirror"));
            });
            ui.end_row();

            ui.label(t("Размытие", "Blur"));
            ui.add(Slider::new(&mut l.blur, 0.0..=60.0).suffix(" px").fixed_decimals(0));
            ui.end_row();

            ui.label(if l.ink { t("Сила линий", "Line strength") } else { t("Яркость", "Brightness") });
            pct_slider(ui, &mut l.brightness, 0.0..=150.0, false);
            ui.end_row();

            ui.label(t("Видимость", "Opacity"));
            pct_slider(ui, &mut l.opacity, 0.0..=100.0, false);
            ui.end_row();

            ui.label(t("Мягкий край", "Soft edge"));
            let edges = !(l.fit == Fit::Cover && l.zoom >= 1.0);
            ui.add_enabled_ui(edges, |ui| pct_slider(ui, &mut l.soft, 0.0..=50.0, false))
                .response
                .on_hover_text(t(
                    "Плавный переход на краях картинки (у «Заполнить» краёв не видно)",
                    "A smooth fade at the picture's edges (a Fill picture shows no edges)",
                ));
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut l.ink, t("Только линии (мел)", "Lines only (chalk)")).on_hover_text(t(
                "Для рисунков на светлой бумаге: бумага исчезает, линии становятся этим цветом",
                "For drawings on light paper: the paper disappears, the lines take this colour",
            ));
            if l.ink {
                ui.color_edit_button_srgb(&mut l.ink_color);
            }
        });
        ui.add_enabled(!l.ink, egui::Checkbox::new(&mut l.screen, t("Чёрный фон — в цвет панели", "Black background → panel colour"))).on_hover_text(t(
            "Для картинок на тёмном фоне: чёрное сливается с панелью Live, видно только светлое",
            "For pictures on a dark background: black melts into Live's panel, only the light parts show",
        ));
        let zone = l.zone.clone();
        if zone != "window" && has_window_bg {
            let p = skin.panels.entry(zone).or_default();
            ui.horizontal(|ui| {
                ui.label(t("Фон всего окна здесь", "Whole-window background here"));
                pct_slider(ui, &mut p.backdrop, 0.0..=100.0, false);
            });
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button(t("Заменить…", "Replace…")).clicked() {
                act = Some(Act::Replace(i));
            }
            if ui
                .button(t("Цвета из неё", "Colours from it"))
                .on_hover_text(t("Подобрать цвета интерфейса по этой картинке", "Pick the interface colours from this picture"))
                .clicked()
            {
                act = Some(Act::ColorsFrom(i));
            }
            if ui.button(t("Дублировать", "Duplicate")).on_hover_text("Ctrl+D").clicked() {
                act = Some(Act::Duplicate(i));
            }
            if ui.button(t("Убрать", "Remove")).on_hover_text("Delete").clicked() {
                act = Some(Act::Remove(i));
            }
        });
        if let Some(z) = new_zone {
            self.zone = z;
        }
        if let Some(a) = act {
            self.apply(a);
        }
    }

    fn colors_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, t("Цвета интерфейса", "Interface colours"), None);
        let source = self
            .sel
            .and_then(|i| self.skin().layers.get(i))
            .or_else(|| self.skin().layers.iter().find(|l| !l.hidden))
            .map(|l| l.picture.clone());
        let mut from_picture = false;
        // edits go to the draft; Live gets them with "Apply"
        let mut colors = self.skin().wanted().clone();
        let c = &mut colors;
        if !c.palette.is_empty() {
            ui.label(
                RichText::new(t("Цвета картинки: щелчок — акцент 1, правый щелчок — акцент 2", "Picture colours: click = accent 1, right-click = accent 2"))
                    .small()
                    .color(pal().weak),
            );
            ui.horizontal_wrapped(|ui| {
                for (rgb, share) in c.palette.clone() {
                    let (r, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                    ui.painter().rect_filled(r, 6.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                    if resp.hovered() {
                        ui.painter().rect_stroke(r, 6.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);
                    }
                    if resp.clicked() {
                        c.accent1 = rgb;
                        c.manual = true;
                    }
                    if resp.secondary_clicked() {
                        c.accent2 = rgb;
                        c.manual = true;
                    }
                    let _ = resp.on_hover_text(format!("{:.0} % {}", share * 100.0, t("картинки", "of the picture")));
                }
            });
            ui.add_space(4.0);
        }
        egui::Grid::new("colors").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(t("Акценты", "Accents"));
            ui.horizontal(|ui| {
                let tip1 = t("Главный: выделение, кнопки, мигающее", "Main: selection, buttons, blinking");
                if ui.color_edit_button_srgb(&mut c.accent1).on_hover_text(tip1).changed() {
                    c.manual = true;
                }
                let tip2 = t("Второй: клипы, выделение времени", "Second: clips, time selection");
                if ui.color_edit_button_srgb(&mut c.accent2).on_hover_text(tip2).changed() {
                    c.manual = true;
                }
            });
            ui.end_row();
            ui.label(t("Оттенок", "Hue"));
            if ui.add(Slider::new(&mut c.hue, 0.0..=360.0).suffix("°").fixed_decimals(0)).changed() {
                c.manual = true;
            }
            ui.end_row();
            ui.label(t("Окраска серого", "Grey tint"));
            let mut tint = c.tint / 0.06 * 100.0;
            if ui.add(Slider::new(&mut tint, 0.0..=100.0).suffix(" %").fixed_decimals(0)).changed() {
                c.tint = tint / 100.0 * 0.06;
                c.manual = true;
            }
            ui.end_row();
            ui.label(t("Темнее", "Darker"));
            if pct_slider(ui, &mut c.depth, 0.0..=100.0, false) {
                c.manual = true;
            }
            ui.end_row();
            ui.label(t("Цветность", "Colourfulness"));
            if pct_slider(ui, &mut c.saturation, 0.0..=100.0, false) {
                c.manual = true;
            }
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(source.is_some(), egui::Button::new(t("Подобрать по картинке", "Match the picture"))).clicked() {
                from_picture = true;
            }
            if ui.button(t("Как у Live", "Like Live")).on_hover_text(t("Обычные цвета Live", "Live's usual colours")).clicked() {
                *c = Colors { manual: true, ..Colors::default() };
            }
        });
        if colors != *self.skin().wanted() {
            self.set_draft(colors);
        }
        if let (true, Some(p)) = (from_picture, source) {
            self.colors_from(&p);
        }
        ui.add_space(10.0);
        section(
            ui,
            t("Текст поверх картинок", "Text over pictures"),
            Some(t("Тёмная кайма вокруг текста Live, чтобы он читался на светлой картинке", "A dark rim around Live's text, so it reads on a light picture")),
        );
        egui::Grid::new("text").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(t("Кайма", "Rim"));
            pct_slider(ui, &mut self.skin_mut().readable, 0.0..=100.0, false);
            ui.end_row();
        });
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(pal().stage.gamma_multiply(palette::STAGE_ALPHA)).inner_margin(Margin::same(16))).show(ctx, |ui| {
            let full = ui.available_rect_before_wrap();
            if let Err(e) = &self.pv {
                ui.label(RichText::new(format!("{}: {e}", t("Превью не работает", "The preview does not work"))).color(Color32::from_rgb(255, 140, 120)));
                return;
            }
            let Some(fs) = self.frame.as_ref().map(|f| f.snap.size) else { return };
            let area = Rect::from_min_max(full.min, pos2(full.max.x, (full.max.y - 84.0).max(full.min.y + 100.0)));
            let aspect = fs[0] as f32 / fs[1].max(1) as f32;
            let mut size = area.size();
            if size.x / size.y > aspect {
                size.x = size.y * aspect;
            } else {
                size.y = size.x / aspect;
            }
            let img = Rect::from_min_size(pos2(area.center().x - size.x / 2.0, area.min.y), size);
            self.render_preview(ctx, size.x * ctx.pixels_per_point());
            self.stage_ui(ui, img, fs);
            let footer = Rect::from_min_max(pos2(full.min.x, img.max.y + 10.0), full.max);
            let mut f = ui.new_child(UiBuilder::new().max_rect(footer).layout(Layout::top_down(Align::Min)));
            self.footer(&mut f);
        });
    }

    fn stage_ui(&mut self, ui: &mut egui::Ui, img: Rect, fs: [u32; 2]) {
        let k = img.width() / fs[0] as f32;
        self.stage = Some((img, k));
        let to_screen = move |r: [f32; 4]| Rect::from_min_size(img.min + vec2(r[0], r[1]) * k, vec2(r[2], r[3]) * k);
        let to_frame = move |p: Pos2| pos2((p.x - img.min.x) / k, (p.y - img.min.y) / k);
        let resp = ui.allocate_rect(img, Sense::click_and_drag());
        let p = pal();
        // the preview gives off a little of the accent's light
        ui.painter().add(egui::Shadow { offset: [0, 0], blur: 44, spread: 0, color: p.accent.gamma_multiply(0.2) }.as_shape(img, CornerRadius::ZERO));
        let painter = ui.painter_at(img.expand(12.0));
        painter.rect_filled(img.expand(1.0), 0.0, p.line);
        if let Some(t) = &self.tex {
            painter.image(t.id(), img, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        let ctx = ui.ctx().clone();
        let files = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let hover = if files {
            ctx.request_repaint();
            self.cursor_in_window(&ctx).filter(|p| img.contains(*p))
        } else {
            resp.hover_pos()
        };
        let fp = hover.map(to_frame);
        let hot_zone = fp.and_then(|p| self.zone_at(p));
        if let Some(f) = &self.frame {
            for (id, r) in &f.snap.zones {
                let hot = hot_zone.as_deref() == Some(id.as_str());
                let sr = to_screen(r.map(|v| v as f32));
                let stroke = if hot { Stroke::new(1.5, p.accent) } else { Stroke::new(1.0, Color32::from_white_alpha(22)) };
                painter.rect_stroke(sr, 0.0, stroke, StrokeKind::Inside);
                if hot && self.drag.is_none() {
                    let note = if files { t(" — бросай сюда", " — drop here") } else { "" };
                    pill(&painter, sr.min + vec2(8.0, 8.0), &format!("{}{note}", config::zone_name(id)));
                }
            }
        }
        // the selected picture, with handles to resize it
        let mut corners: Vec<(usize, Pos2)> = Vec::new();
        if let Some(i) = self.sel {
            if let Some((pl, zr)) = self.placed(i) {
                let pr = to_screen(pl);
                let zs = to_screen(zr);
                painter.rect_stroke(pr, 0.0, Stroke::new(1.0, p.accent.gamma_multiply(0.45)), StrokeKind::Middle);
                painter.with_clip_rect(zs.intersect(img)).rect_stroke(pr, 0.0, Stroke::new(2.0, p.accent), StrokeKind::Middle);
                if self.skin().layers[i].fit != Fit::Cover {
                    for (n, c) in [pr.left_top(), pr.right_top(), pr.left_bottom(), pr.right_bottom()].into_iter().enumerate() {
                        if img.expand(6.0).contains(c) {
                            let h = Rect::from_center_size(c, vec2(10.0, 10.0));
                            painter.rect_filled(h, 2.0, Color32::WHITE);
                            painter.rect_stroke(h, 2.0, Stroke::new(1.5, p.accent), StrokeKind::Inside);
                            corners.push((n, c));
                        }
                    }
                }
            }
        }
        let corner_at = |p: Pos2| corners.iter().find(|(_, c)| c.distance(p) < 10.0).map(|(n, _)| *n);
        let resize_icon = |left: bool, top: bool| if left == top { CursorIcon::ResizeNwSe } else { CursorIcon::ResizeNeSw };
        let cursor = match self.drag {
            Some(Drag::Move { .. }) => Some(CursorIcon::Grabbing),
            Some(Drag::Resize { left, top, .. }) => Some(resize_icon(left, top)),
            None => hover.and_then(|p| match corner_at(p) {
                Some(n) => Some(resize_icon(n % 2 == 0, n < 2)),
                None => fp.and_then(|q| self.layer_at(q)).map(|i| if Some(i) == self.sel { CursorIcon::Grab } else { CursorIcon::PointingHand }),
            }),
        };
        if let Some(c) = cursor {
            ctx.set_cursor_icon(c);
        }

        if resp.drag_started() {
            if let Some(o) = ctx.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
                self.start_drag(to_frame(o), corner_at(o));
            }
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.update_drag(to_frame(p));
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
        }
        if resp.clicked() {
            if let Some(q) = resp.interact_pointer_pos().map(to_frame) {
                match self.layer_at(q) {
                    Some(i) => self.select(i),
                    None => {
                        self.sel = None;
                        self.zone = self.zone_at(q).unwrap_or_else(|| "window".into());
                    }
                }
            }
        }
        if resp.double_clicked() {
            if let Some(q) = resp.interact_pointer_pos().map(to_frame) {
                if self.layer_at(q).is_none() {
                    let zone = self.zone_at(q).unwrap_or_else(|| "window".into());
                    for f in pick_files() {
                        self.add_picture(&f, &zone);
                    }
                }
            }
        }
        if resp.hovered() && self.drag.is_none() {
            let notches: f32 = ctx.input(|i| {
                i.raw
                    .events
                    .iter()
                    .map(|e| match e {
                        Event::MouseWheel { unit, delta, .. } => match unit {
                            MouseWheelUnit::Line => delta.y,
                            MouseWheelUnit::Point => delta.y / 50.0,
                            MouseWheelUnit::Page => delta.y * 4.0,
                        },
                        _ => 0.0,
                    })
                    .sum()
            });
            if let (true, Some(i), Some(q)) = (notches != 0.0, self.sel, fp) {
                self.zoom_at(i, q, 1.1f32.powf(notches));
            }
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        let en = lang::english();
        let status = match &self.frame {
            Some(f) if f.stand_in => t(
                "Live ещё не открывался при Ableton Skin Lab — превью на примере окна Live. Открой Live, и здесь будет твоё окно.",
                "Ableton Skin Lab has not seen Live yet — the preview uses a sample Live window. Open Live and your own window shows here.",
            )
            .to_string(),
            Some(f) => match f.taken.and_then(|at| at.elapsed().ok()).map(|a| a.as_secs()) {
                Some(s) if s < 90 => t("Превью по свежему снимку твоего Live", "Preview from a fresh snapshot of your Live").into(),
                Some(s) if s < 90 * 60 && en => format!("Preview from a snapshot of your Live, {} min ago", s / 60),
                Some(s) if s < 90 * 60 => format!("Превью по снимку твоего Live {} мин назад", s / 60),
                Some(s) if en => format!("Preview from a snapshot of your Live, {} h ago", s / 3600),
                Some(s) => format!("Превью по снимку твоего Live {} ч назад", s / 3600),
                None => t("Превью по снимку твоего Live", "Preview from a snapshot of your Live").into(),
            },
            None => String::new(),
        };
        let dim = pal().weak;
        ui.label(RichText::new(status).color(dim));
        ui.label(
            RichText::new(t(
                "Щелчок — выбрать картинку · тяни — двигать · уголки — размер · колёсико — крупнее/мельче · \
                 двойной щелчок по пустой панели — добавить · файлы можно бросать прямо сюда",
                "Click to select a picture · drag to move · corners to resize · wheel to scale · \
                 double-click an empty panel to add · drop files right here",
            ))
            .small()
            .color(dim),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.label(RichText::new(format!("{APP_NAME} {} · {}", update::VERSION, t("сделал музыкант", "made by musician"))).small().color(dim));
            ui.hyperlink_to(RichText::new("blueberry").small(), AUTHOR_URL).on_hover_text(AUTHOR_URL);
            ui.label(RichText::new("·").small().color(dim));
            if self.update == Upd::Checking(true) {
                ui.spinner();
                ui.label(RichText::new(t("Проверяю обновления…", "Checking for updates…")).small().color(dim));
            } else if ui.link(RichText::new(t("Проверить обновления", "Check for updates")).small()).clicked() {
                self.check_update(true);
            }
        });
        if let Some((msg, at)) = &self.toast {
            if at.elapsed() < Duration::from_secs(6) {
                ui.label(RichText::new(msg.as_str()).color(pal().accent2));
            }
        }
    }

    fn update_dialog(&mut self, ctx: &egui::Context) {
        if !self.update_open {
            return;
        }
        let Some(r) = self.release.clone() else {
            self.update_open = false;
            return;
        };
        let busy = !matches!(self.update, Upd::Idle | Upd::Checking(_));
        let (mut install, mut close) = (false, false);
        let modal = egui::Modal::new(Id::new("update")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(format!("{} {}", t("Новая версия", "New version"), r.version));
            ui.label(RichText::new(format!("{} {}", t("Сейчас у тебя", "You have"), update::VERSION)).color(pal().weak));
            ui.add_space(6.0);
            let notes = notes_text(&r.notes);
            if !notes.is_empty() {
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| ui.label(notes));
                ui.add_space(8.0);
            }
            if let Some(e) = &self.update_error {
                ui.label(RichText::new(format!("{}: {e}", t("Не получилось обновить", "The update did not work"))).color(Color32::from_rgb(255, 140, 120)));
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                if busy {
                    ui.spinner();
                    ui.label(if self.update == Upd::Downloading { t("Скачиваю…", "Downloading…") } else { t("Перезапускаю…", "Restarting…") });
                } else {
                    install = ui.add(primary(t("Обновить и перезапустить", "Update and restart"), 0.0)).clicked();
                    close = ui.button(t("Позже", "Later")).clicked();
                }
            });
        });
        if modal.should_close() && !busy {
            close = true;
        }
        if install {
            self.install_update(r);
        } else if close {
            self.update_open = false;
            self.update_error = None;
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let current = self.skin().name.clone();
        let Some(dialog) = &mut self.dialog else { return };
        let focus = std::mem::take(&mut self.focus_dialog);
        let (mut ok, mut cancel) = (false, false);
        let modal = egui::Modal::new(Id::new("dialog")).show(ctx, |ui| {
            ui.set_width(380.0);
            let (title, button) = match dialog {
                Dialog::New(_) => (t("Новый скин", "New skin"), t("Создать", "Create")),
                Dialog::Rename(_) => (t("Переименовать скин", "Rename skin"), t("Готово", "Done")),
                Dialog::Delete => (t("Удалить скин?", "Delete the skin?"), t("Удалить", "Delete")),
            };
            ui.heading(title);
            ui.add_space(6.0);
            match dialog {
                Dialog::New(name) | Dialog::Rename(name) => {
                    let r = ui.add(egui::TextEdit::singleline(name).hint_text(t("Название скина", "Skin name")).desired_width(f32::INFINITY));
                    if focus {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        ok = true;
                    }
                }
                Dialog::Delete => {
                    ui.label(if lang::english() {
                        format!("“{current}” goes away with its picture layout. The pictures themselves stay on disk.")
                    } else {
                        format!("«{current}» пропадёт вместе с раскладкой картинок. Сами картинки останутся на диске.")
                    });
                }
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.add(primary(button, 0.0)).clicked() {
                    ok = true;
                }
                if ui.button(t("Отмена", "Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if ok {
            match self.dialog.take() {
                Some(Dialog::New(name)) => {
                    let name = self.unique_name(&name);
                    let mut skin = Skin::new(&name);
                    // the colours Live has now, so that switching to the new skin leaves Live be
                    skin.colors = Colors { palette: Vec::new(), from: None, manual: false, ..self.skin().colors.clone() };
                    self.cfg.skins.push(skin);
                    self.cfg.active = self.cfg.skins.len() - 1;
                    self.sel = None;
                }
                Some(Dialog::Rename(name)) => {
                    let name = name.trim().to_string();
                    if !name.is_empty() && name != current {
                        let name = self.unique_name(&name);
                        self.skin_mut().name = name;
                    }
                }
                Some(Dialog::Delete) => {
                    if self.cfg.skins.len() > 1 {
                        self.cfg.skins.remove(self.cfg.active);
                        self.cfg.active = self.cfg.active.min(self.cfg.skins.len() - 1);
                        self.sel = None;
                    }
                }
                None => {}
            }
        } else if cancel {
            self.dialog = None;
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || ctx.wants_keyboard_input() || ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let Some(i) = self.sel else { return };
        let (del, dup, esc, left, right, up, down, shift) = ctx.input_mut(|inp| {
            (
                inp.consume_key(Modifiers::NONE, Key::Delete),
                inp.consume_key(Modifiers::COMMAND, Key::D),
                inp.consume_key(Modifiers::NONE, Key::Escape),
                inp.key_pressed(Key::ArrowLeft),
                inp.key_pressed(Key::ArrowRight),
                inp.key_pressed(Key::ArrowUp),
                inp.key_pressed(Key::ArrowDown),
                inp.modifiers.shift,
            )
        });
        if del {
            self.remove_layer(i);
        } else if dup {
            self.duplicate_layer(i);
        } else if esc {
            self.sel = None;
        } else {
            let step = if shift { 10.0 } else { 1.0 };
            let d = [(right as i32 - left as i32) as f32 * step, (down as i32 - up as i32) as f32 * step];
            if d != [0.0, 0.0] {
                self.nudge(i, d);
            }
        }
    }

    fn shot_step(&mut self, ctx: &egui::Context) {
        let frames = self.frames;
        let ready = self.tex.is_some();
        let Some(shot) = &mut self.shot else { return };
        ctx.request_repaint();
        let got = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = got {
            let px: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(out) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, px) {
                if let Err(e) = out.save(&shot.path) {
                    log!("shot: {e}");
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !shot.asked && frames > 45 && ready {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            shot.asked = true;
        } else if frames > 900 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for Editor {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;
        self.thumb_budget = 2;
        self.poll(ctx);
        self.take_dropped(ctx);
        self.receive_done();
        self.receive_update(ctx);
        self.follow_colors(ctx);
        palette::light_selection(ctx);
        self.backdrop(ctx);
        self.top_bar(ctx);
        self.side_panel(ctx);
        // the side panel's changes reach this frame's preview
        self.sync_preview();
        self.central(ctx);
        self.dialogs(ctx);
        self.update_dialog(ctx);
        self.shortcuts(ctx);
        // changes made on the preview itself show in the next frame
        let skin = &self.cfg.skins[self.cfg.active];
        if config::fnv(&serde_json::to_vec(skin).unwrap_or_default()) != self.skin_hash {
            ctx.request_repaint();
        }
        let closing = ctx.input(|i| i.viewport().close_requested());
        self.save_if_changed(ctx, closing);
        self.shot_step(ctx);
        ctx.request_repaint_after(Duration::from_millis(700));
    }
}
