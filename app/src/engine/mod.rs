//! The background engine. Sits in the tray, waits for Live, keeps a click-through overlay window
//! glued right above Live's main window and draws the skin's pictures into it (only where Live
//! shows its panel background), and keeps Live's colour theme in step with the skin.

mod capture;
pub mod gpu;
pub mod scene;

use crate::config::{self, Colors, Config, Snapshot, State, APP_NAME, ENGINE_CLASS, WM_EXIT, WM_RELOAD, WM_SNAPSHOT, ZONES};
use crate::lang::{self, t};
use crate::live::{self, LiveWin, Uia, Zones};
use crate::log;
use capture::Capture;
use gpu::Gpu;
use scene::Scene;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, NOTIFY_ICON_MESSAGE,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Our two theme files: a changed theme always goes into the one Live is not using, because
/// Live only re-reads a theme when it is selected again.
const SLOT_A: &str = "Skin Lab A";
const SLOT_B: &str = "Skin Lab B";
const WM_TRAY: u32 = WM_APP + 10;

// set from window procedures and WinEvent callbacks, handled in the main loop
static RELOAD: AtomicBool = AtomicBool::new(true);
static MOVED: AtomicBool = AtomicBool::new(true);
static SIZING: AtomicBool = AtomicBool::new(false);
static TRAY_MENU: AtomicBool = AtomicBool::new(false);
static TRAY_OPEN: AtomicBool = AtomicBool::new(false);
static TRAY_READD: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);
static SNAPSHOT: AtomicBool = AtomicBool::new(false);
static LIVE_HWND: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

enum ToUia {
    Live(Option<LiveWin>),
    Poll,
    Apply(String),
}

enum FromUia {
    Zones(Option<Zones>),
    Applied(Result<String, String>),
}

/// UI Automation runs on its own thread: its calls go into Live's process and may take a while.
fn uia_thread(rx: Receiver<ToUia>, tx: Sender<FromUia>, wake: isize) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let uia = match Uia::new() {
        Ok(u) => u,
        Err(e) => {
            log!("UI Automation unavailable: {e}");
            return;
        }
    };
    let send = |m| {
        let _ = tx.send(m);
        unsafe {
            let _ = SetEvent(HANDLE(wake as _));
        }
    };
    let mut live: Option<LiveWin> = None;
    let mut last: Option<Zones> = None;
    let mut next = Instant::now();
    loop {
        match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Ok(ToUia::Live(l)) => {
                live = l;
                last = None;
                next = Instant::now();
                if live.is_none() {
                    send(FromUia::Zones(None));
                }
            }
            Ok(ToUia::Poll) => next = Instant::now(),
            Ok(ToUia::Apply(name)) => {
                let t0 = Instant::now();
                let r = match &live {
                    Some(l) => uia.apply_theme(l, &name),
                    None => Err("Live is not running".into()),
                };
                log!("theme '{name}': {:?} in {} ms", r, t0.elapsed().as_millis());
                send(FromUia::Applied(r));
                next = Instant::now() + Duration::from_millis(300);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() < next {
            continue;
        }
        let Some(l) = &live else {
            next = Instant::now() + Duration::from_secs(3600);
            continue;
        };
        // a failed query (Live busy) keeps the last known panels
        if let Ok(z) = uia.zones(l.hwnd) {
            if Some(z) != last {
                last = Some(z);
                send(FromUia::Zones(Some(z)));
            }
        }
        let focused = live::pid_of(unsafe { GetForegroundWindow() }) == l.pid;
        next = Instant::now() + Duration::from_millis(if focused { 250 } else { 1000 });
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_RELOAD => RELOAD.store(true, Relaxed),
        WM_SNAPSHOT => SNAPSHOT.store(true, Relaxed),
        WM_TRAY => match lp.0 as u32 {
            WM_LBUTTONUP => TRAY_OPEN.store(true, Relaxed),
            WM_RBUTTONUP | WM_CONTEXTMENU => TRAY_MENU.store(true, Relaxed),
            _ => {}
        },
        WM_DISPLAYCHANGE | WM_DPICHANGED => MOVED.store(true, Relaxed),
        WM_ENDSESSION | WM_EXIT => QUIT.store(true, Relaxed),
        _ if msg != 0 && msg == TASKBAR_CREATED.load(Relaxed) => TRAY_READD.store(true, Relaxed),
        _ => return unsafe { DefWindowProcW(h, msg, wp, lp) },
    }
    LRESULT(0)
}

unsafe extern "system" fn on_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
        return;
    }
    let is_live = hwnd.0 as isize == LIVE_HWND.load(Relaxed);
    match event {
        EVENT_SYSTEM_FOREGROUND => MOVED.store(true, Relaxed),
        EVENT_SYSTEM_MOVESIZESTART if is_live => SIZING.store(true, Relaxed),
        EVENT_SYSTEM_MOVESIZEEND if is_live => {
            SIZING.store(false, Relaxed);
            MOVED.store(true, Relaxed);
        }
        _ if is_live => MOVED.store(true, Relaxed),
        _ => {}
    }
}

/// A skin theme selected in Live: what the frames look like.
#[derive(Clone)]
struct Shown {
    hash: u64,
    key: [u8; 3],
    colors: Colors,
}

struct Attached {
    win: LiveWin,
    cap: Option<Capture>,
    hooks: Vec<HWINEVENTHOOK>,
    since: Instant,
    /// Live's window as DWM draws it, screen pixels
    bounds: RECT,
    /// where our overlay window currently is
    placed: RECT,
    shown: bool,
    /// our theme selected in this Live, if known
    theme: Option<Shown>,
    /// UI Automation has answered at least once: Live's UI is up
    ui_ready: bool,
    mismatch_since: Option<Instant>,
    sizes_logged: (u32, u32, u32, u32),
}

struct Engine {
    cfg: Config,
    state: State,
    gpu: Gpu,
    overlay: HWND,
    msg: HWND,
    icon: HICON,
    to_uia: Sender<ToUia>,
    from_uia: Receiver<FromUia>,
    frame_event: HANDLE,
    wake_event: HANDLE,
    live: Option<Attached>,
    next_find: Instant,
    zones: Option<Zones>,
    scene: Scene,
    dirty: bool,
    /// theme being selected in Live right now; without a skin theme: putting the user's back
    applying: Option<(String, Option<Shown>)>,
    failures: u32,
    next_apply: Instant,
    /// the theme the skin wants (hash, 0 = the user's own) and since when
    want_since: Option<(u64, Instant)>,
    fg_hook: HWINEVENTHOOK,
}

pub fn run() {
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\SkinLabEngine"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            config::notify_engine();
            return;
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        match Engine::new() {
            Ok(mut e) => e.run(),
            Err(e) => log!("engine failed to start: {e}"),
        }
    }
}

fn register_class(name: PCWSTR) -> windows::core::Result<HINSTANCE> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: name,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        Ok(hinst)
    }
}

impl Engine {
    fn new() -> windows::core::Result<Engine> {
        unsafe {
            TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Relaxed);
            let hinst = register_class(ENGINE_CLASS)?;
            register_class(w!("SkinLabOverlay"))?;
            let msg = CreateWindowExW(WINDOW_EX_STYLE(0), ENGINE_CLASS, w!("Skin Lab engine"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None)?;
            // click-through, never activated, not in the taskbar or Alt+Tab; its pixels come
            // from DirectComposition with per-pixel alpha
            let overlay = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
                w!("SkinLabOverlay"),
                w!("Skin Lab overlay"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hinst),
                None,
            )?;
            SetLayeredWindowAttributes(overlay, COLORREF(0), 255, LWA_ALPHA)?;
            let gpu = Gpu::new(Some(overlay))?;
            let frame_event = CreateEventW(None, false, false, None)?;
            let wake_event = CreateEventW(None, false, false, None)?;
            let (to_uia, rx) = channel();
            let (tx, from_uia) = channel();
            let wake = wake_event.0 as isize;
            std::thread::spawn(move || uia_thread(rx, tx, wake));
            let fg_hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            let icon = tray_icon(GetSystemMetrics(SM_CXSMICON).max(16) as u32);
            let e = Engine {
                cfg: Config::default(),
                state: config::load(&config::state_path()),
                gpu,
                overlay,
                msg,
                icon,
                to_uia,
                from_uia,
                frame_event,
                wake_event,
                live: None,
                next_find: Instant::now(),
                zones: None,
                scene: Scene::default(),
                dirty: true,
                applying: None,
                failures: 0,
                next_apply: Instant::now(),
                want_since: None,
                fg_hook,
            };
            e.tray(NIM_ADD);
            log!("engine started");
            Ok(e)
        }
    }

    fn run(&mut self) {
        let mut msg = MSG::default();
        while !QUIT.load(Relaxed) {
            let timeout = if self.live.is_some() { 50 } else { 1000 };
            unsafe {
                MsgWaitForMultipleObjectsEx(Some(&[self.frame_event, self.wake_event]), timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        QUIT.store(true, Relaxed);
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if !QUIT.load(Relaxed) {
                self.tick();
            }
        }
        self.shutdown();
    }

    fn shutdown(&mut self) {
        log!("engine stopping");
        self.detach();
        unsafe {
            let _ = UnhookWinEvent(self.fg_hook);
        }
        self.tray(NIM_DELETE);
        unsafe {
            let _ = DestroyWindow(self.overlay);
            let _ = DestroyWindow(self.msg);
        }
    }

    fn tick(&mut self) {
        if RELOAD.swap(false, Relaxed) {
            self.reload();
        }
        if TRAY_READD.swap(false, Relaxed) {
            self.tray(NIM_ADD);
        }
        if TRAY_OPEN.swap(false, Relaxed) {
            config::start_editor();
        }
        if TRAY_MENU.swap(false, Relaxed) {
            self.tray_menu();
        }
        while let Ok(m) = self.from_uia.try_recv() {
            match m {
                FromUia::Zones(z) => self.set_zones(z),
                FromUia::Applied(r) => self.applied(r),
            }
        }
        self.track_live();
        self.update_overlay();
        self.manage_theme();
        if SNAPSHOT.swap(false, Relaxed) {
            self.write_snapshot();
        }
    }

    fn reload(&mut self) {
        self.cfg = config::load(&config::config_path());
        lang::set(&self.cfg.lang);
        self.prepare();
        log!(
            "settings: {} skins, active '{}', enabled {}",
            self.cfg.skins.len(),
            self.cfg.active_skin().map(|s| s.name.as_str()).unwrap_or("-"),
            self.cfg.enabled
        );
    }

    /// Builds the active skin's theme and loads its pictures onto the GPU.
    fn prepare(&mut self) {
        let skin = self.cfg.active_skin().cloned();
        self.scene.prepare(&self.gpu, skin.as_ref());
        if let Some(b) = &self.scene.built {
            log!("theme background #{:02x}{:02x}{:02x}", b.key[0], b.key[1], b.key[2]);
        }
        self.dirty = true;
    }

    fn save_config(&mut self) {
        if let Err(e) = config::save(&config::config_path(), &self.cfg) {
            log!("saving settings: {e}");
        }
        self.prepare();
    }

    fn save_state(&self) {
        if let Err(e) = config::save(&config::state_path(), &self.state) {
            log!("saving state: {e}");
        }
    }

    fn track_live(&mut self) {
        if let Some(a) = &self.live {
            if !unsafe { IsWindow(Some(a.win.hwnd)) }.as_bool() {
                self.detach();
            }
        }
        if self.live.is_none() && Instant::now() >= self.next_find {
            self.next_find = Instant::now() + Duration::from_millis(1500);
            if let Some(w) = live::find() {
                self.attach(w);
            }
        }
    }

    fn attach(&mut self, win: LiveWin) {
        log!("Live found: '{}' pid {} ({})", live::window_text(win.hwnd), win.pid, win.exe.display());
        if let Some(d) = live::themes_dir(&win.exe) {
            let d = d.to_string_lossy().into_owned();
            if self.state.themes_dir.as_deref() != Some(d.as_str()) {
                self.state.themes_dir = Some(d);
                self.save_state();
            }
            if self.scene.built.is_none() {
                self.prepare();
            }
        }
        let cap = match Capture::new(&self.gpu.winrt, win.hwnd, self.frame_event) {
            Ok(c) => Some(c),
            Err(e) => {
                log!("capture failed: {e}");
                None
            }
        };
        let mut hooks = Vec::new();
        for (a, b) in [
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        ] {
            let h = unsafe { SetWinEventHook(a, b, None, Some(on_event), win.pid, 0, WINEVENT_OUTOFCONTEXT) };
            if !h.0.is_null() {
                hooks.push(h);
            }
        }
        LIVE_HWND.store(win.hwnd.0 as isize, Relaxed);
        let _ = self.to_uia.send(ToUia::Live(Some(win.clone())));
        self.live = Some(Attached {
            win,
            cap,
            hooks,
            since: Instant::now(),
            bounds: RECT::default(),
            placed: RECT::default(),
            shown: false,
            theme: None,
            ui_ready: false,
            mismatch_since: None,
            sizes_logged: (0, 0, 0, 0),
        });
        self.failures = 0;
        MOVED.store(true, Relaxed);
    }

    fn detach(&mut self) {
        let Some(a) = self.live.take() else { return };
        log!("Live gone");
        for h in a.hooks {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        LIVE_HWND.store(0, Relaxed);
        let _ = self.to_uia.send(ToUia::Live(None));
        self.zones = None;
        self.gpu.forget_frame();
        unsafe {
            let _ = ShowWindow(self.overlay, SW_HIDE);
        }
    }

    fn set_zones(&mut self, z: Option<Zones>) {
        if let (Some(a), Some(_)) = (self.live.as_mut(), z) {
            a.ui_ready = true;
        }
        if self.zones == z {
            return;
        }
        self.zones = z;
        self.dirty = true;
        if let Some(z) = z {
            log!("panels: {:?}", self.zone_rects(&z));
        }
    }

    /// Panels in frame pixels (x, y, w, h).
    fn zone_rects(&self, z: &Zones) -> Vec<(String, [i32; 4])> {
        let b = self.live.as_ref().map(|a| a.bounds).unwrap_or_default();
        ZONES
            .iter()
            .filter_map(|id| z.get(id).map(|r| (id.to_string(), [r.left - b.left, r.top - b.top, r.right - r.left, r.bottom - r.top])))
            .collect()
    }

    /// The raw Live frame and its panels, for the editor's preview.
    fn write_snapshot(&self) {
        let (Some(a), Some((w, h, px))) = (&self.live, self.gpu.read_frame()) else { return };
        let snap = Snapshot {
            size: [w, h],
            key: self.frame_key().unwrap_or_default(),
            theme_ok: a.theme.is_some(),
            colors: a.theme.as_ref().map(|t| t.colors.clone()),
            zones: self.zones.map(|z| self.zone_rects(&z).into_iter().collect()).unwrap_or_default(),
        };
        let (json, raw) = config::snapshot_paths();
        if std::fs::write(&raw, &px).is_ok() {
            let _ = config::save(&json, &snap);
        }
    }

    fn update_overlay(&mut self) {
        let work = self.cfg.enabled && self.scene.has_pictures() && self.scene.built.is_some();
        let Some(a) = self.live.as_mut() else { return };
        let hwnd = a.win.hwnd;
        if let Some(cap) = a.cap.as_mut() {
            if cap.take(&mut self.gpu).is_some() {
                self.dirty = true;
            }
        }
        if MOVED.swap(false, Relaxed) {
            if let Some(b) = live::frame_bounds(hwnd) {
                if b != a.bounds {
                    a.bounds = b;
                    self.dirty = true;
                    let _ = self.to_uia.send(ToUia::Poll);
                }
            }
        }
        let (fw, fh) = self.gpu.ui_size;
        let (bw, bh) = ((a.bounds.right - a.bounds.left) as u32, (a.bounds.bottom - a.bounds.top) as u32);
        if (fw, fh, bw, bh) != a.sizes_logged && fw > 0 {
            log!("frame {fw}x{fh}, window {bw}x{bh} at {},{}", a.bounds.left, a.bounds.top);
            a.sizes_logged = (fw, fh, bw, bh);
        }
        // right after a resize the frames still have the old size: wait for them, but if the two
        // never agree, trust the frames
        let mut size_ok = fw > 0 && (fw, fh) == (bw, bh);
        if !size_ok && fw > 0 {
            let since = *a.mismatch_since.get_or_insert_with(Instant::now);
            size_ok = since.elapsed() > Duration::from_secs(2);
        } else {
            a.mismatch_since = None;
        }
        let visible = unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() } && !live::cloaked(hwnd) && !SIZING.load(Relaxed);
        let want = visible && size_ok && work;
        if !want {
            if a.shown {
                unsafe {
                    let _ = ShowWindow(self.overlay, SW_HIDE);
                }
                a.shown = false;
            }
            return;
        }
        let rect = RECT { left: a.bounds.left, top: a.bounds.top, right: a.bounds.left + fw as i32, bottom: a.bounds.top + fh as i32 };
        if self.dirty {
            self.dirty = false;
            if let Err(e) = self.render() {
                log!("render failed: {e}");
            }
        }
        let Some(a) = self.live.as_mut() else { return };
        unsafe {
            if rect != a.placed {
                let _ = SetWindowPos(self.overlay, None, rect.left, rect.top, fw as i32, fh as i32, SWP_NOZORDER | SWP_NOACTIVATE);
                a.placed = rect;
            }
            if !a.shown {
                let _ = ShowWindow(self.overlay, SW_SHOWNOACTIVATE);
                a.shown = true;
            }
        }
        place_above(self.overlay, hwnd);
    }

    /// The panel background in Live's frames: that of the skin theme Live shows (while a new
    /// one is on its way, the pictures stay where they were).
    fn frame_key(&self) -> Option<[u8; 3]> {
        let shown = self.live.as_ref().and_then(|a| a.theme.as_ref()).map(|t| t.key);
        shown.or(self.scene.built.as_ref().map(|b| b.key))
    }

    fn render(&mut self) -> windows::core::Result<()> {
        let (Some(a), Some(key)) = (&self.live, self.frame_key()) else { return Ok(()) };
        let origin = (a.bounds.left, a.bounds.top);
        let zones = self.zones;
        let draws = self.scene.draws(self.gpu.ui_size, key, |id| {
            zones.and_then(|z| z.get(id)).map(|r| {
                [(r.left - origin.0) as f32, (r.top - origin.1) as f32, (r.right - r.left) as f32, (r.bottom - r.top) as f32]
            })
        });
        let p = self.scene.combine_params(key);
        self.gpu.render(&draws, &p)
    }

    /// Keeps Live's selected theme equal to the active skin's (or puts the user's own back when
    /// the skin is switched off).
    fn manage_theme(&mut self) {
        let Some(a) = self.live.as_mut() else { return };
        if self.applying.is_some() || Instant::now() < self.next_apply || !a.ui_ready || a.since.elapsed() < Duration::from_secs(2) {
            return;
        }
        let Some(dir) = live::themes_dir(&a.win.exe) else { return };
        let want = if self.cfg.enabled { self.scene.built.clone() } else { None };
        // selecting a theme opens Live's Settings for a moment: wait until the colours stop
        // changing (someone dragging a slider in the editor)
        let hash = want.as_ref().map(|t| t.hash).unwrap_or(0);
        if self.want_since.map(|(h, _)| h) != Some(hash) {
            self.want_since = Some((hash, Instant::now()));
        }
        if self.want_since.is_some_and(|(_, t)| t.elapsed() < Duration::from_millis(1500)) {
            return;
        }
        match want {
            Some(t) => {
                let shown = Shown { hash: t.hash, key: t.key, colors: self.cfg.active_skin().map(|s| s.colors.clone()).unwrap_or_default() };
                if self.state.slot.is_some() && self.state.hash == t.hash {
                    if a.theme.as_ref().is_some_and(|s| s.hash == t.hash) {
                        return;
                    }
                    // a fresh Live: it remembers the theme it had when it quit
                    if live::prefs_selected(self.state.slot.as_deref().unwrap_or_default()) {
                        log!("Live already has {:?}", self.state.slot);
                        a.theme = Some(shown);
                        self.dirty = true;
                        return;
                    }
                }
                let slot = if self.state.slot.as_deref() == Some(SLOT_A) { SLOT_B } else { SLOT_A };
                if let Err(e) = std::fs::write(dir.join(format!("{slot}.ask")), &t.text) {
                    log!("writing theme: {e}");
                    self.next_apply = Instant::now() + Duration::from_secs(30);
                    return;
                }
                self.applying = Some((slot.to_string(), Some(shown)));
                let _ = self.to_uia.send(ToUia::Apply(slot.to_string()));
            }
            None => {
                if self.state.slot.is_none() {
                    return;
                }
                let back = self.state.user_theme.clone().unwrap_or_else(|| "Default".into());
                self.applying = Some((back.clone(), None));
                let _ = self.to_uia.send(ToUia::Apply(back));
            }
        }
    }

    fn applied(&mut self, r: Result<String, String>) {
        let Some((name, shown)) = self.applying.take() else { return };
        let dir = self.live.as_ref().and_then(|a| live::themes_dir(&a.win.exe));
        match r {
            // Live had this slot selected already and doesn't re-read a selected theme, so it
            // still shows the slot's older colours: the next try takes the other slot
            Ok(previous) if previous == name && shown.is_some() => {
                log!("Live already had '{name}' selected");
                self.state.slot = Some(name);
                self.state.hash = 0;
                self.save_state();
            }
            Ok(previous) => {
                if !previous.is_empty() && !previous.starts_with("Skin Lab") {
                    self.state.user_theme = Some(previous);
                }
                let remove = |slot: &str| {
                    if let Some(d) = &dir {
                        let _ = std::fs::remove_file(d.join(format!("{slot}.ask")));
                    }
                };
                match shown {
                    Some(t) => {
                        remove(if name == SLOT_A { SLOT_B } else { SLOT_A });
                        self.state.slot = Some(name);
                        self.state.hash = t.hash;
                        if let Some(a) = self.live.as_mut() {
                            a.theme = Some(t);
                        }
                    }
                    None => {
                        remove(SLOT_A);
                        remove(SLOT_B);
                        self.state.slot = None;
                        self.state.hash = 0;
                        if let Some(a) = self.live.as_mut() {
                            a.theme = None;
                        }
                    }
                }
                self.dirty = true;
                self.failures = 0;
                self.save_state();
            }
            Err(e) => {
                self.failures += 1;
                log!("selecting theme '{name}' failed ({}): {e}", self.failures);
                let wait = if self.failures < 3 { 10 } else { 300 };
                self.next_apply = Instant::now() + Duration::from_secs(wait);
            }
        }
    }

    fn tray(&self, op: NOTIFY_ICON_MESSAGE) {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.msg,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        let tip: Vec<u16> = APP_NAME.encode_utf16().collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        unsafe {
            let _ = Shell_NotifyIconW(op, &nid);
        }
    }

    fn tray_menu(&mut self) {
        unsafe {
            let Ok(m) = CreatePopupMenu() else { return };
            let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
            let item = |flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
                let h = HSTRING::from(text);
                let _ = AppendMenuW(m, flags, id, PCWSTR(h.as_ptr()));
            };
            item(MF_STRING, 1, t("Открыть Ableton Skin Lab", "Open Ableton Skin Lab"));
            item(MF_STRING | check(self.cfg.enabled), 2, t("Скин включён", "Skin on"));
            if !self.cfg.skins.is_empty() {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
                for (i, s) in self.cfg.skins.iter().enumerate() {
                    item(MF_STRING | check(i == self.cfg.active), 100 + i, &s.name);
                }
            }
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            item(MF_STRING | check(config::autostart()), 3, t("Запускать вместе с Windows", "Start with Windows"));
            item(MF_STRING, 4, t("Выход", "Exit"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // without this the menu would not close when clicking elsewhere
            let _ = SetForegroundWindow(self.msg);
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, self.msg, None).0 as usize;
            let _ = PostMessageW(Some(self.msg), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(m);
            match cmd {
                1 => config::start_editor(),
                2 => {
                    self.cfg.enabled = !self.cfg.enabled;
                    self.save_config();
                }
                3 => config::set_autostart(!config::autostart()),
                4 => QUIT.store(true, Relaxed),
                i if i >= 100 && i - 100 < self.cfg.skins.len() => {
                    self.cfg.active = i - 100;
                    self.save_config();
                }
                _ => {}
            }
        }
    }
}

/// The app icon for the tray.
fn tray_icon(size: u32) -> HICON {
    use windows::Win32::Graphics::Gdi::{CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
    let px = crate::icon::rgba(size);
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else { return HICON::default() };
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, px.len());
        for (d, s) in dst.chunks_exact_mut(4).zip(px.chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

/// Puts the overlay directly above Live's main window: under every window that is above Live
/// (other apps, Live's own plug-in windows, menus and dialogs), so it never covers them.
fn place_above(overlay: HWND, live: HWND) {
    unsafe {
        let prev = GetWindow(live, GW_HWNDPREV).ok();
        if prev == Some(overlay) {
            return;
        }
        let topmost = |h: HWND| GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0;
        let after = match prev {
            Some(p) if !topmost(p) || topmost(live) => p,
            _ if topmost(live) => HWND_TOPMOST,
            _ => HWND_TOP,
        };
        let _ = SetWindowPos(overlay, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
    }
}
