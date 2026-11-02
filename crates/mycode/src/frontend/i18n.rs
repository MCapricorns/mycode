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

#[cfg(target_os = "macos")]
mod macos_language;

#[cfg(any(not(windows), test))]
fn locale_is_chinese(locale: &str) -> bool {
    locale
        .trim()
        .split(['-', '_', '.', '@'])
        .next()
        .is_some_and(|language| language.eq_ignore_ascii_case("zh"))
}

/// The active language id: 0 = English, 1 = Simplified Chinese.
static LANGUAGE: AtomicU8 = AtomicU8::new(0);

/// Whether the UI renders in Simplified Chinese.
#[must_use]
pub fn is_chinese() -> bool {
    LANGUAGE.load(Ordering::Relaxed) == 1
}

/// Picks one of the two spellings for the active language.
#[must_use]
pub fn t(en: &'static str, zh: &'static str) -> &'static str {
    if is_chinese() { zh } else { en }
}

/// Applies a configured language id (`auto`, `en`, `zh`). `auto` follows the
/// operating system's UI language.
pub fn apply_language(configured: &str) {
    let chinese = match configured {
        "zh" => true,
        "en" => false,
        _ => system_is_chinese(),
    };
    LANGUAGE.store(u8::from(chinese), Ordering::Relaxed);
}

/// Use the native UI-language preference on Windows and macOS. Environment
/// locale variables remain a fallback for other desktop sessions.
fn system_is_chinese() -> bool {
    #[cfg(windows)]
    {
        // GetUserDefaultUILanguage returns a LANGID; Chinese primary
        // languages are 0x04 (zh-*). SUBLANG bits live in the high byte.
        // SAFETY: The process-local query takes no pointers or owned handles.
        let language = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        language & 0x03FF == 0x0004
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "macos")]
        if let Some(chinese) = macos_language::preferred_is_chinese() {
            return chinese;
        }
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok())
            .find(|value| !value.trim().is_empty())
            .is_some_and(|locale| locale_is_chinese(&locale))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn locale_matching_uses_a_language_subtag_not_an_arbitrary_prefix() {
        for locale in ["zh", "zh_CN.UTF-8", "zh-Hans-CN", "ZH_TW", " zh-Hant "] {
            assert!(super::locale_is_chinese(locale), "{locale}");
        }
        for locale in ["en-US", "C.UTF-8", "", "zhuang", "zhx"] {
            assert!(!super::locale_is_chinese(locale), "{locale}");
        }
    }
}
