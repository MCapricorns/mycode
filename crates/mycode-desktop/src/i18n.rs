//! UI localization: English and Simplified Chinese.
//!
//! The active language is a process-global choice loaded from settings and
//! flipped live from the General page. Call sites carry both spellings so the
//! pair stays next to its use site:
//!
//! `t("Subagents", "子代理")`
//!
//! Strings that are diagnostics (bridge errors, ledger failures) stay
//! English; only human chrome is localized.

use std::sync::atomic::{AtomicU8, Ordering};

/// The active language id: 0 = English, 1 = Simplified Chinese.
static LANGUAGE: AtomicU8 = AtomicU8::new(0);

/// Whether the UI renders in Simplified Chinese.
#[must_use]
pub(crate) fn is_chinese() -> bool {
    LANGUAGE.load(Ordering::Relaxed) == 1
}

/// Picks one of the two spellings for the active language.
#[must_use]
pub(crate) fn t(en: &'static str, zh: &'static str) -> &'static str {
    if is_chinese() { zh } else { en }
}

/// Applies a configured language id (`auto`, `en`, `zh`). `auto` follows the
/// operating system's UI language.
pub(crate) fn apply_language(configured: &str) {
    let chinese = match configured {
        "zh" => true,
        "en" => false,
        _ => system_is_chinese(),
    };
    LANGUAGE.store(u8::from(chinese), Ordering::Relaxed);
}

/// Resolves what the General page shows for a configured id.
#[must_use]
pub(crate) fn effective_language_id(configured: &str) -> &'static str {
    match configured {
        "zh" => "zh",
        "en" => "en",
        _ => {
            if system_is_chinese() {
                "zh"
            } else {
                "en"
            }
        }
    }
}

/// The operating system UI language, best effort. Windows reports its
/// default UI language; elsewhere the `LANG` convention is the only signal.
fn system_is_chinese() -> bool {
    #[cfg(windows)]
    {
        // GetUserDefaultUILanguage returns a LANGID; Chinese primary
        // languages are 0x04 (zh-*). SUBLANG bits live in the high byte.
        let language = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        language & 0x00FF == 0x0004
    }
    #[cfg(not(windows))]
    {
        std::env::var("LANG")
            .map(|lang| lang.to_ascii_lowercase().starts_with("zh"))
            .unwrap_or(false)
    }
}
