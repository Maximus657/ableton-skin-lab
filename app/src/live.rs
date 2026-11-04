//! Ableton Live from the outside: finding its main window, the rectangles of its panels and
//! picking a theme in its Settings (both through UI Automation), and the theme it remembers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::core::{Interface, BOOL, BSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::{VARIANT, VT_BSTR, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetMenu, GetMenuItemCount, GetMenuItemID, GetMenuStringW, GetSubMenu, GetWindow,
    GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, GW_OWNER, MF_BYPOSITION,
    WM_CLOSE, WM_COMMAND,
};

/// Live's main window.
#[derive(Clone, Debug)]
pub struct LiveWin {
    pub hwnd: HWND,
    pub pid: u32,
    pub exe: PathBuf,
}
unsafe impl Send for LiveWin {}

pub fn window_text(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn process_exe(pid: u32) -> Option<PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])))
    }
}

pub fn pid_of(h: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Visible top-level windows, optionally only those of one process.
pub fn top_windows(pid: Option<u32>) -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lp.0 as *mut Vec<HWND>) };
        if unsafe { IsWindowVisible(h) }.as_bool() {
            out.push(h);
        }
        BOOL(1)
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
    }
    match pid {
        Some(p) => all.into_iter().filter(|&h| pid_of(h) == p).collect(),
        None => all,
    }
}

/// Live's main window: the top-level window of an "Ableton Live*.exe" that has a menu bar.
pub fn find() -> Option<LiveWin> {
    // for testing the overlay without Live: attach to any window whose title contains this
    if let Some(t) = std::env::var_os("SKINLAB_TARGET").map(|t| t.to_string_lossy().into_owned()) {
        let h = top_windows(None).into_iter().find(|&h| window_text(h).contains(&t))?;
        let pid = pid_of(h);
        return Some(LiveWin { hwnd: h, pid, exe: process_exe(pid)? });
    }
    for h in top_windows(None) {
        unsafe {
            if GetWindow(h, GW_OWNER).is_ok() || GetMenu(h).0.is_null() {
                continue;
            }
        }
        if !window_text(h).contains("Ableton Live") {
            continue;
        }
        let pid = pid_of(h);
        let Some(exe) = process_exe(pid) else { continue };
        let name = exe.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
        if name.starts_with("ableton live") {
            return Some(LiveWin { hwnd: h, pid, exe });
        }
    }
    None
}

pub fn themes_dir(exe: &Path) -> Option<PathBuf> {
    let d = exe.parent()?.parent()?.join("Resources").join("Themes");
    d.is_dir().then_some(d)
}

/// The window as DWM draws it (no invisible resize borders): what Windows Graphics Capture grabs.
pub fn frame_bounds(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as _, std::mem::size_of::<RECT>() as u32).ok()?;
    }
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

/// Hidden by DWM, e.g. on another virtual desktop.
pub fn cloaked(h: HWND) -> bool {
    let mut v = 0u32;
    unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut v as *mut _ as _, 4) }.is_ok() && v != 0
}

/// Menu command of Options > Settings… (1166 in Live 12.4).
fn settings_command(live: HWND) -> u32 {
    unsafe {
        let bar = GetMenu(live);
        for i in 0..GetMenuItemCount(Some(bar)).max(0) {
            let sub = GetSubMenu(bar, i);
            if sub.0.is_null() {
                continue;
            }
            for j in 0..GetMenuItemCount(Some(sub)).max(0) {
                let mut buf = [0u16; 128];
                let n = GetMenuStringW(sub, j as u32, Some(&mut buf), MF_BYPOSITION);
                let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
                if text.starts_with("Settings") || text.starts_with("Preferences") {
                    return GetMenuItemID(sub, j);
                }
            }
        }
    }
    1166
}

/// Whether Live's Preferences.cfg (written when Live quits) has this theme selected: there the
/// name is stored as an int32 length + UTF-16, followed by the "use default theme" byte.
pub fn prefs_selected(name: &str) -> bool {
    let Some(appdata) = std::env::var_os("APPDATA") else { return false };
    let Ok(rd) = std::fs::read_dir(PathBuf::from(appdata).join("Ableton")) else { return false };
    let newest = rd
        .flatten()
        .map(|e| e.path().join("Preferences").join("Preferences.cfg"))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .max_by_key(|(t, _)| *t);
    let Some((_, path)) = newest else { return false };
    let Ok(bytes) = std::fs::read(path) else { return false };
    let mut pat = (name.encode_utf16().count() as u32).to_le_bytes().to_vec();
    pat.extend(name.encode_utf16().flat_map(|u| u.to_le_bytes()));
    bytes
        .windows(pat.len())
        .position(|w| w == pat.as_slice())
        .is_some_and(|i| bytes.get(i + pat.len()) == Some(&0))
}

fn settings_window(pid: u32) -> Option<HWND> {
    top_windows(Some(pid)).into_iter().find(|&h| {
        let t = window_text(h);
        t == "Settings" || t == "Preferences"
    })
}

fn wait_for<T>(limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if t0.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn var_str(s: &str) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_BSTR;
        inner.Anonymous.bstrVal = std::mem::ManuallyDrop::new(BSTR::from(s));
    }
    v
}

fn var_i32(x: i32) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = x;
    }
    v
}

/// Panel rectangles in screen pixels. Arrangement: lanes left of the track headers, tracks =
/// headers + mixer. Session: lanes = clip slots and scenes, tracks = the mixer row below them.
/// Side: the help / file manager panel right of them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Zones {
    pub browser: Option<RECT>,
    pub lanes: Option<RECT>,
    pub tracks: Option<RECT>,
    pub device: Option<RECT>,
    pub side: Option<RECT>,
}

impl Zones {
    pub fn get(&self, id: &str) -> Option<RECT> {
        match id {
            "browser" => self.browser,
            "lanes" => self.lanes,
            "tracks" => self.tracks,
            "device" => self.device,
            "side" => self.side,
            _ => None,
        }
    }
}

pub struct Uia {
    a: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    all: IUIAutomationCondition,
}

struct Child {
    id: String,
    name: String,
    rect: RECT,
    el: IUIAutomationElement,
}

impl Uia {
    /// Needs COM initialised on the calling thread.
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let a: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            if let Ok(a2) = a.cast::<IUIAutomation2>() {
                // a busy Live must not hang us for long
                let _ = a2.SetConnectionTimeout(2000);
                let _ = a2.SetTransactionTimeout(3000);
            }
            let cache = a.CreateCacheRequest()?;
            cache.AddProperty(UIA_AutomationIdPropertyId)?;
            cache.AddProperty(UIA_NamePropertyId)?;
            cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
            let all = a.CreateTrueCondition()?;
            Ok(Uia { a, cache, all })
        }
    }

    /// Direct children with id, name and rectangle, fetched in one cross-process call.
    fn children(&self, e: &IUIAutomationElement) -> Vec<Child> {
        unsafe {
            let Ok(arr) = e.FindAllBuildCache(TreeScope_Children, &self.all, &self.cache) else { return Vec::new() };
            (0..arr.Length().unwrap_or(0))
                .filter_map(|i| {
                    let el = arr.GetElement(i).ok()?;
                    Some(Child {
                        id: el.CachedAutomationId().map(|b| b.to_string()).unwrap_or_default(),
                        name: el.CachedName().map(|b| b.to_string()).unwrap_or_default(),
                        rect: el.CachedBoundingRectangle().unwrap_or_default(),
                        el,
                    })
                })
                .collect()
        }
    }

    pub fn zones(&self, live: HWND) -> windows::core::Result<Zones> {
        let root = unsafe { self.a.ElementFromHandle(live)? };
        let mut z = Zones::default();
        let mut arrangement = None;
        let mut session = None;
        let children: Vec<Child> = self.children(&root).into_iter().filter(|c| c.rect.right > c.rect.left && c.rect.bottom > c.rect.top).collect();
        for c in &children {
            let r = c.rect;
            match c.id.as_str() {
                "ContentBrowser" => z.browser = Some(r),
                "ArrangementView" => arrangement = Some(c),
                "SessionView" => session = Some(c),
                "TrackView" | "ClipView" => z.device = Some(r),
                _ if c.name.ends_with("Detail") => z.device = Some(r),
                _ => {}
            }
        }
        // the help view has no id and its name follows what it shows: the tall panel past the
        // right edge of the main view
        if let Some(m) = arrangement.or(session).map(|c| c.rect) {
            z.side = children.iter().map(|c| c.rect).find(|r| r.left >= m.right - 4 && (r.bottom - r.top) * 2 >= m.bottom - m.top);
        }
        if let Some(a) = arrangement {
            let r = a.rect;
            let split = self.children(&a.el).iter().find(|c| c.id == "ArrangementView.ArrangerHeaderManager").map(|c| c.rect.left);
            match split {
                Some(x) if x > r.left && x < r.right => {
                    z.lanes = Some(RECT { right: x, ..r });
                    z.tracks = Some(RECT { left: x, ..r });
                }
                _ => z.lanes = Some(r),
            }
        } else if let Some(s) = session {
            let r = s.rect;
            let top = self
                .children(&s.el)
                .iter()
                .filter(|c| c.id.ends_with(".Mixer") && c.rect.bottom > c.rect.top)
                .map(|c| c.rect.top)
                .min();
            match top {
                Some(y) if y > r.top && y < r.bottom => {
                    z.lanes = Some(RECT { bottom: y, ..r });
                    z.tracks = Some(RECT { top: y, ..r });
                }
                _ => z.lanes = Some(r),
            }
        }
        Ok(z)
    }

    fn by_id(&self, el: &IUIAutomationElement, id: &str) -> Option<IUIAutomationElement> {
        unsafe {
            let c = self.a.CreatePropertyCondition(UIA_AutomationIdPropertyId, &var_str(id)).ok()?;
            el.FindFirst(TreeScope_Descendants, &c).ok()
        }
    }

    /// Selects a theme in Live: opens Settings (its menu command, no keystrokes), picks the
    /// theme on the Theme & Colors page and closes Settings again. Returns the theme that was
    /// selected before. Live rescans its Themes folder whenever Settings opens.
    pub fn apply_theme(&self, live: &LiveWin, name: &str) -> Result<String, String> {
        let was_open = settings_window(live.pid);
        if let Some(s) = was_open {
            // an already open Settings window has not seen our new file: reopen it
            unsafe {
                let _ = PostMessageW(Some(s), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            wait_for(Duration::from_millis(1500), || settings_window(live.pid).is_none().then_some(()));
        }
        unsafe { PostMessageW(Some(live.hwnd), WM_COMMAND, WPARAM(settings_command(live.hwnd) as usize), LPARAM(0)) }
            .map_err(|e| format!("команда настроек: {e}"))?;
        let s = wait_for(Duration::from_secs(4), || settings_window(live.pid)).ok_or("окно настроек Live не открылось")?;
        let result = self.pick_theme(live, s, name);
        std::thread::sleep(Duration::from_millis(150));
        if was_open.is_none() {
            unsafe {
                let _ = PostMessageW(Some(s), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        result
    }

    fn pick_theme(&self, live: &LiveWin, settings: HWND, name: &str) -> Result<String, String> {
        unsafe {
            let el = self.a.ElementFromHandle(settings).map_err(|e| e.to_string())?;
            let tab = wait_for(Duration::from_secs(2), || self.by_id(&el, "ThemeColorsTabButton")).ok_or("в настройках нет вкладки Theme & Colors")?;
            tab.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                .and_then(|p| p.Invoke())
                .map_err(|e| format!("вкладка Theme & Colors: {e}"))?;
            let chooser = wait_for(Duration::from_secs(2), || self.by_id(&el, "ThemeColors.ThemeChooser")).ok_or("в настройках нет списка тем")?;
            let previous = chooser
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .and_then(|p| p.CurrentValue())
                .map(|b| b.to_string())
                .unwrap_or_default();
            if previous == name {
                return Ok(previous);
            }
            let expand = || {
                let _ = chooser.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId).and_then(|p| p.Expand());
            };
            let a = self.a.CreatePropertyCondition(UIA_NamePropertyId, &var_str(name)).map_err(|e| e.to_string())?;
            let b = self.a.CreatePropertyCondition(UIA_NamePropertyId, &var_str(&format!("{name}, checked"))).map_err(|e| e.to_string())?;
            let t = self.a.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_i32(UIA_MenuItemControlTypeId.0)).map_err(|e| e.to_string())?;
            let cond = self.a.CreateAndCondition(&t, &self.a.CreateOrCondition(&a, &b).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            expand();
            let t0 = Instant::now();
            let mut next = 1000;
            loop {
                // the list is a popup window of Live's; while it is being built an element can
                // vanish mid-search, which just means "try again"
                for h in top_windows(Some(live.pid)) {
                    if h == live.hwnd {
                        continue;
                    }
                    let Ok(w) = self.a.ElementFromHandle(h) else { continue };
                    if let Ok(item) = w.FindFirst(TreeScope_Descendants, &cond) {
                        return item
                            .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                            .and_then(|p| p.Invoke())
                            .map(|_| previous)
                            .map_err(|e| format!("выбор темы: {e}"));
                    }
                }
                let ms = t0.elapsed().as_millis();
                if ms > 4000 {
                    return Err(format!("темы «{name}» нет в списке Live"));
                }
                if ms > next {
                    // right after a theme switch Live rebuilds the page and may swallow Expand()
                    expand();
                    next += 1000;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
