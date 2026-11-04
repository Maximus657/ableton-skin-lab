//! Settings shared by the editor and the engine, and the small bits of process plumbing around
//! them: where the files live, telling the engine to reload, autostart, the log.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_APP, WM_CLOSE};

use crate::lang::t;

pub const APP_NAME: &str = "Ableton Skin Lab";
/// the editor's window: winit's class and our title
pub const EDITOR_CLASS: PCWSTR = w!("Window Class");
pub const EDITOR_TITLE: PCWSTR = w!("Ableton Skin Lab");
pub const ENGINE_CLASS: PCWSTR = w!("SkinLabEngine");
pub const WM_RELOAD: u32 = WM_APP + 1;
pub const WM_SNAPSHOT: u32 = WM_APP + 2;
/// the engine quits without touching Live's theme (an update is taking its place)
pub const WM_EXIT: u32 = WM_APP + 3;

/// Where a picture can go: Live's panels, and the whole window. A picture is cut to its panel.
pub const ZONES: [&str; 6] = ["window", "browser", "lanes", "tracks", "device", "side"];

pub fn zone_name(id: &str) -> &'static str {
    match id {
        "window" => t("Всё окно", "Whole window"),
        "browser" => t("Браузер", "Browser"),
        "lanes" => t("Аранжировка и клипы", "Arrangement and clips"),
        "tracks" => t("Дорожки и микшер", "Tracks and mixer"),
        "device" => t("Устройства", "Devices"),
        "side" => t("Справка справа", "Help panel on the right"),
        _ => "?",
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum Fit {
    /// fills the panel, cropping what sticks out
    Cover,
    /// the whole picture is visible; the rest of the panel is filled from its blurred edges
    Contain,
    /// a sticker: just the picture, `zoom` times the largest size that fits the panel
    Free,
}

/// One picture in a skin. Pictures are drawn bottom to top, each cut to its panel.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Layer {
    pub picture: String,
    pub zone: String,
    pub fit: Fit,
    /// where it sits in the panel: 0 = against the left/top edge, 1 = against the right/bottom
    /// one (beyond that it sticks out and is cut off); keeps its place when the panel resizes
    pub anchor: [f32; 2],
    pub zoom: f32,
    pub mirror: bool,
    /// quarter turns clockwise
    pub rotate: u8,
    /// blur radius in screen pixels
    pub blur: f32,
    pub brightness: f32,
    pub opacity: f32,
    /// soft edge, as a share of the picture
    pub soft: f32,
    /// line art: only the lines stay, in `ink_color`; the paper goes transparent
    pub ink: bool,
    pub ink_color: [u8; 3],
    /// black takes the panel's colour: a dark picture background melts into the panel
    pub screen: bool,
    pub hidden: bool,
}

impl Default for Layer {
    fn default() -> Self {
        Layer {
            picture: String::new(),
            zone: "window".into(),
            fit: Fit::Cover,
            anchor: [0.5, 0.5],
            zoom: 1.0,
            mirror: false,
            rotate: 0,
            blur: 0.0,
            brightness: 0.6,
            opacity: 1.0,
            soft: 0.15,
            ink: false,
            ink_color: [224, 224, 224],
            screen: false,
            hidden: false,
        }
    }
}

/// Per-panel settings.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Panel {
    /// how much of the whole-window backgrounds shows in this panel: 1 = all, 0 = plain colour
    pub backdrop: f32,
}

impl Default for Panel {
    fn default() -> Self {
        Panel { backdrop: 1.0 }
    }
}

/// Interface colours. The greys of Live's stock dark theme keep their lightness and take `hue`
/// with strength `tint`; Live's orange and cyan accents become `accent1` and `accent2`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Colors {
    /// degrees
    pub hue: f32,
    /// OKLCH chroma of the greys, 0..0.06
    pub tint: f32,
    pub accent1: [u8; 3],
    pub accent2: [u8; 3],
    /// darker backgrounds: 0 = Live's own, 1 = nearly black (text keeps its lightness)
    pub depth: f32,
    /// how colourful the rest of Live stays (meters, play and record, automation): 0 = grey
    pub saturation: f32,
    /// the picture's main colours and their shares, only for showing
    pub palette: Vec<([u8; 3], f32)>,
    /// file name of the picture the colours came from
    pub from: Option<String>,
    /// set by hand: a new picture no longer replaces them
    pub manual: bool,
}

impl Default for Colors {
    fn default() -> Self {
        Colors {
            hue: 0.0,
            tint: 0.0,
            accent1: [0xff, 0xad, 0x56],
            accent2: [0x03, 0xc3, 0xd5],
            depth: 0.0,
            saturation: 1.0,
            palette: Vec::new(),
            from: None,
            manual: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Skin {
    pub name: String,
    pub colors: Colors,
    /// bottom first
    pub layers: Vec<Layer>,
    pub panels: BTreeMap<String, Panel>,
    /// 0..1: how much the pictures give way to the plain panel colour right around Live's text
    /// and icons, so they stay readable on a light picture
    pub readable: f32,
    /// colours being tried in the editor; Live keeps `colors` until they are applied
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<Colors>,
}

impl Default for Skin {
    fn default() -> Self {
        Skin::new(t("Мой скин", "My skin"))
    }
}

impl Skin {
    pub fn new(name: &str) -> Self {
        Skin { name: name.to_string(), colors: Colors::default(), layers: Vec::new(), panels: BTreeMap::new(), readable: 1.0, draft: None }
    }

    /// The colours the editor shows: the draft, if any.
    pub fn wanted(&self) -> &Colors {
        self.draft.as_ref().unwrap_or(&self.colors)
    }
}

/// Written by the editor (and the tray menu), read by the engine.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub active: usize,
    pub skins: Vec<Skin>,
    /// interface language: "ru", "en", or empty for the language of Windows
    pub lang: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { enabled: true, active: 0, skins: Vec::new(), lang: String::new() }
    }
}

impl Config {
    pub fn active_skin(&self) -> Option<&Skin> {
        self.skins.get(self.active).or(self.skins.first())
    }
}

/// Written only by the engine: what it did to Live.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct State {
    /// Live's theme before Skin Lab took over, put back when the skin is switched off
    pub user_theme: Option<String>,
    /// our theme file (without .ask) currently selected in Live, and a hash of its contents
    pub slot: Option<String>,
    pub hash: u64,
    /// Live's Themes folder, found from the running Live
    pub themes_dir: Option<String>,
}

/// A frame of the running Live (raw, without pictures) with its panels, for the editor's preview.
/// The pixels (BGRA rows) are in a file next to this one.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Snapshot {
    pub size: [u32; 2],
    /// panel background of the skin theme the engine draws for
    pub key: [u8; 3],
    /// that theme is selected in Live (otherwise the frame shows another theme)
    pub theme_ok: bool,
    /// the colours of that theme
    pub colors: Option<Colors>,
    /// panels in frame pixels (x, y, w, h)
    pub zones: BTreeMap<String, [i32; 4]>,
}

pub fn dir() -> PathBuf {
    let d = PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("SkinLab");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn config_path() -> PathBuf {
    dir().join("config.json")
}

pub fn state_path() -> PathBuf {
    dir().join("state.json")
}

pub fn snapshot_paths() -> (PathBuf, PathBuf) {
    (dir().join("live.json"), dir().join("live.raw"))
}

pub fn pictures_dir() -> PathBuf {
    let d = dir().join("pictures");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write-then-rename, so a reader never sees half a file.
pub fn save<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)
}

pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Copies a picture into Skin Lab's own folder, so skins survive the original being moved.
pub fn import_picture(src: &Path) -> Result<String, String> {
    if src.starts_with(pictures_dir()) {
        return Ok(src.to_string_lossy().into_owned());
    }
    let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let stem: String = src
        .file_stem()
        .map(|s| {
            let s = s.to_string_lossy().replace(' ', "_");
            s.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(40).collect()
        })
        .unwrap_or_default();
    let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "png".into());
    let dst = pictures_dir().join(format!("{stem}-{:08x}.{ext}", fnv(&bytes) as u32));
    if !dst.exists() {
        std::fs::write(&dst, &bytes).map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    Ok(dst.to_string_lossy().into_owned())
}

/// A skin made from one picture: it covers the whole window and gives the colours.
pub fn new_skin_from_picture(name: &str, src: &Path) -> Result<(), String> {
    let pic = import_picture(src)?;
    let img = crate::theme::open_image(Path::new(&pic))?;
    let file = src.file_name().map(|f| f.to_string_lossy().into_owned());
    let mut skin = Skin::new(name);
    skin.colors = crate::theme::colors_from_image(&img, file);
    let mut layer = crate::theme::suggest_layer(&img, 2.5);
    layer.picture = pic;
    skin.layers.push(layer);

    let mut cfg: Config = load(&config_path());
    match cfg.skins.iter().position(|s| s.name == name) {
        Some(i) => {
            cfg.skins[i] = skin;
            cfg.active = i;
        }
        None => {
            cfg.skins.push(skin);
            cfg.active = cfg.skins.len() - 1;
        }
    }
    cfg.enabled = true;
    save(&config_path(), &cfg).map_err(|e| e.to_string())
}

pub fn engine_running() -> bool {
    unsafe { FindWindowW(ENGINE_CLASS, PCWSTR::null()).is_ok() }
}

/// Asks a running engine to re-read the settings. False when no engine is running.
pub fn notify_engine() -> bool {
    unsafe {
        match FindWindowW(ENGINE_CLASS, PCWSTR::null()) {
            Ok(h) => PostMessageW(Some(h), WM_RELOAD, WPARAM(0), LPARAM(0)).is_ok(),
            Err(_) => false,
        }
    }
}

/// Asks the engine to save a snapshot of Live for the preview.
pub fn request_snapshot() -> bool {
    unsafe {
        match FindWindowW(ENGINE_CLASS, PCWSTR::null()) {
            Ok(h) => PostMessageW(Some(h), WM_SNAPSHOT, WPARAM(0), LPARAM(0)).is_ok(),
            Err(_) => false,
        }
    }
}

/// Asks a running engine to quit (leaving Live's theme as it is) and waits up to `wait` for it
/// to be gone. True when no engine runs any more.
pub fn stop_engine(wait: std::time::Duration) -> bool {
    unsafe {
        if let Ok(h) = FindWindowW(ENGINE_CLASS, PCWSTR::null()) {
            let _ = PostMessageW(Some(h), WM_EXIT, WPARAM(0), LPARAM(0));
        }
    }
    let until = Instant::now() + wait;
    loop {
        if !engine_running() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub fn editor_open() -> bool {
    unsafe { FindWindowW(EDITOR_CLASS, EDITOR_TITLE).is_ok() }
}

/// Asks an open editor window to close.
pub fn close_editor() {
    unsafe {
        if let Ok(h) = FindWindowW(EDITOR_CLASS, EDITOR_TITLE) {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn start_engine() {
    spawn_self(&["--background"]);
}

pub fn start_editor() {
    spawn_self(&[]);
}

pub fn spawn_self(args: &[&str]) {
    if let Ok(exe) = std::env::current_exe() {
        spawn(&exe, args);
    }
}

pub fn spawn(exe: &Path, args: &[&str]) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x8;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    // leave the parent's job object if we may, so closing a terminal does not take us along
    let ok = std::process::Command::new(exe)
        .args(args)
        .creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .is_ok();
    if !ok {
        let _ = std::process::Command::new(&exe).args(args).creation_flags(DETACHED_PROCESS).spawn();
    }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("SkinLab");

fn run_command() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    format!("\"{}\" --background", exe.display())
}

/// Whether Windows starts this very exe's engine at logon.
pub fn autostart() -> bool {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if r.is_err() {
        return false;
    }
    let n = (size as usize / 2).saturating_sub(1).min(buf.len());
    String::from_utf16_lossy(&buf[..n]).eq_ignore_ascii_case(&run_command())
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let cmd = HSTRING::from(run_command());
            let bytes = (cmd.len() + 1) * 2;
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ.0, Some(cmd.as_ptr().cast()), bytes as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

static ROLE: OnceLock<&'static str> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

pub fn set_role(role: &'static str) {
    let _ = ROLE.set(role);
    START.get_or_init(Instant::now);
    if role == "engine" {
        let p = dir().join("skinlab.log");
        if p.metadata().map(|m| m.len() > 512 * 1024).unwrap_or(false) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

pub fn log(msg: &str) {
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let role = ROLE.get().copied().unwrap_or("?");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("skinlab.log")) {
        let _ = writeln!(f, "[{role} {t:8.3}] {msg}");
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::config::log(&format!($($t)*)) };
}
