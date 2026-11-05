// Ableton Skin Lab: pictures and colour themes for Ableton Live.
//   AbletonSkinLab.exe               editor (starts the engine if it is not running)
//   AbletonSkinLab.exe --background  engine: tray icon, waits for Live, draws the pictures over it
//   AbletonSkinLab.exe --new-skin <name> <picture>   make a skin from one picture and switch to it
//   AbletonSkinLab.exe --update      install the newest release from GitHub, if there is a newer one
#![windows_subsystem = "windows"]

mod config;
mod editor;
mod engine;
mod icon;
mod lang;
mod live;
mod palette;
mod preview;
mod theme;
mod update;

use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

fn main() {
    // every coordinate we deal with (Live's window, UI Automation, our overlay) is in physical pixels
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--background") => {
            config::set_role("engine");
            engine::run();
        }
        Some("--preview") => {
            config::set_role("cli");
            if let Err(e) = preview::cli(&args[1..]) {
                config::log(&format!("preview failed: {e}"));
            }
        }
        Some("--new-skin") if args.len() >= 3 => {
            config::set_role("cli");
            match config::new_skin_from_picture(&args[1], std::path::Path::new(&args[2])) {
                Ok(()) => config::log("skin created"),
                Err(e) => config::log(&format!("new skin failed: {e}")),
            }
            config::notify_engine();
        }
        Some("--update") => {
            config::set_role("cli");
            match update::check().and_then(|r| r.map(|r| update::install(&r)).transpose()) {
                Ok(Some(exe)) => {
                    let editor = config::editor_open();
                    config::close_editor();
                    update::restart(&exe, editor);
                }
                Ok(None) => config::log(&format!("{} is the newest version", update::VERSION)),
                Err(e) => config::log(&format!("update failed: {e}")),
            }
        }
        Some("--updated") => {
            config::set_role("editor");
            editor::run(true);
        }
        _ => {
            config::set_role("editor");
            editor::run(false);
        }
    }
}
