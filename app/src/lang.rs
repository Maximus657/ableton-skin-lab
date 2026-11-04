//! The interface language: Russian or English, as set in the editor or, by default, as Windows
//! speaks.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// `lang` from the settings: "ru", "en", or anything else for the language of Windows.
pub fn set(lang: &str) {
    let en = match lang {
        "ru" => false,
        "en" => true,
        _ => !windows_is_russian(),
    };
    ENGLISH.store(en, Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Relaxed)
}

/// The text in the interface language.
pub fn t(ru: &'static str, en: &'static str) -> &'static str {
    if english() {
        en
    } else {
        ru
    }
}

fn windows_is_russian() -> bool {
    // the primary language of the user's display language; 0x19 = LANG_RUSSIAN
    unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() & 0x3ff == 0x19 }
}
