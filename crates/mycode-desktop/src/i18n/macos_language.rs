//! Finder launches do not need a shell locale. Read the user's ordered
//! language preferences through CoreFoundation instead of relying on LANG.
use std::ffi::{c_char, c_void};

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFLocaleCopyPreferredLanguages() -> *const c_void;
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(value: *const c_void);
}

pub(super) fn preferred_is_chinese() -> Option<bool> {
    const UTF8: u32 = 0x0800_0100;
    // SAFETY: CopyPreferredLanguages returns an owned CFArray of CFStrings.
    // The array remains retained while its first string is borrowed. The
    // count check bounds index 0, and the writable buffer matches its length.
    // Every non-null owned array is released exactly once after the read;
    // the borrowed string is never released independently.
    unsafe {
        let languages = CFLocaleCopyPreferredLanguages();
        if languages.is_null() {
            return None;
        }
        let result = if CFArrayGetCount(languages) > 0 {
            let first = CFArrayGetValueAtIndex(languages, 0);
            let mut buffer = [0_u8; 128];
            if !first.is_null()
                && CFStringGetCString(
                    first,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as isize,
                    UTF8,
                ) != 0
            {
                let end = buffer
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(buffer.len());
                std::str::from_utf8(&buffer[..end])
                    .ok()
                    .map(super::locale_is_chinese)
            } else {
                None
            }
        } else {
            None
        };
        CFRelease(languages);
        result
    }
}
