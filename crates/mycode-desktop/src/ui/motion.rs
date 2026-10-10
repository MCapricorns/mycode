//! Short opacity fades for view changes.
//!
//! Layout stays put. Only opacity moves, and the animation caps its frame
//! rate so a settings switch does not repaint the desk on every refresh.
//! Reduced motion skips the tween: the element is inserted at full opacity
//! and no animation frames are scheduled.
use gpui_kit::{
    Animation, AnimationExt, AnyElement, ElementId, IntoElement, Styled, ease_out_quint,
};

const ENTER_MS: u64 = 170;

/// Whether a one-shot opacity fade should tween.
#[must_use]
pub(super) fn opacity_fade_plays(reduce_motion: bool) -> bool {
    !reduce_motion
}

/// Fades `element` in once. The id restarts the fade when the page changes;
/// the same id leaves a finished fade at full opacity. Reduced motion mounts
/// the element at full opacity instead of tweening.
pub(super) fn fade_in(
    id: impl Into<ElementId>,
    reduce_motion: bool,
    element: impl FadeTarget,
) -> AnyElement {
    if opacity_fade_plays(reduce_motion) {
        FadeTarget::fade_in(element, id.into()).into_any_element()
    } else {
        element.into_any_element()
    }
}

/// Elements that can take the shared enter fade.
pub(super) trait FadeTarget: AnimationExt + Styled + IntoElement + Sized + 'static {
    /// Runs the one-shot opacity fade.
    fn fade_in(self, id: ElementId) -> impl IntoElement {
        self.with_animation(
            id,
            Animation::new(std::time::Duration::from_millis(ENTER_MS))
                .with_easing(ease_out_quint())
                .with_max_fps(30.),
            |this, delta| this.opacity(delta),
        )
    }
}

impl<T> FadeTarget for T where T: AnimationExt + Styled + IntoElement + Sized + 'static {}

/// Operating-system reduce-motion preference, when this host can read one.
///
/// `None` leaves the app flag alone (the probe is missing or failed).
/// Windows reports client-area animations; macOS reports the workspace
/// accessibility display option. GPUI's own [`gpui_kit::App::reduce_motion`]
/// is what the fades read after this value is applied.
#[must_use]
pub(super) fn system_prefers_reduced_motion() -> Option<bool> {
    #[cfg(windows)]
    {
        windows_prefers_reduced_motion()
    }
    #[cfg(target_os = "macos")]
    {
        macos_prefers_reduced_motion()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// Maps a Windows `SPI_GETCLIENTAREAANIMATION` result.
///
/// The API returns whether animations are enabled. A failed query yields
/// `None` so a previous app flag is kept.
#[must_use]
#[cfg(any(windows, test))]
fn reduce_motion_from_windows_animation(query_ok: bool, animations_enabled: bool) -> Option<bool> {
    query_ok.then_some(!animations_enabled)
}

/// Maps a macOS `accessibilityDisplayShouldReduceMotion` result.
///
/// A missing class yields `None`.
#[must_use]
#[cfg(any(target_os = "macos", test))]
fn reduce_motion_from_macos_flag(class_found: bool, reduce_motion: bool) -> Option<bool> {
    class_found.then_some(reduce_motion)
}

#[cfg(windows)]
fn windows_prefers_reduced_motion() -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };

    // Default to "enabled" so a failed write-back cannot look like reduce-motion.
    let mut enabled: i32 = 1;
    // SAFETY: SPI_GETCLIENTAREAANIMATION requires `pvParam` to point at a
    // BOOL (4-byte int). The call has no other preconditions.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut enabled as *mut i32 as *mut core::ffi::c_void,
            0,
        )
    };
    reduce_motion_from_windows_animation(ok != 0, enabled != 0)
}

#[cfg(target_os = "macos")]
fn macos_prefers_reduced_motion() -> Option<bool> {
    use std::ffi::{c_char, c_void};

    #[link(name = "objc")]
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;

        #[link_name = "objc_msgSend"]
        fn msg_send_id(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void;

        #[link_name = "objc_msgSend"]
        fn msg_send_bool(receiver: *mut c_void, selector: *mut c_void) -> u8;
    }

    // SAFETY: both selectors take no arguments. `sharedWorkspace` returns the
    // process-wide NSWorkspace. `accessibilityDisplayShouldReduceMotion`
    // returns a BOOL. A null class or workspace is treated as "no probe".
    unsafe {
        let class = objc_getClass(c"NSWorkspace".as_ptr());
        if class.is_null() {
            return reduce_motion_from_macos_flag(false, false);
        }
        let workspace = msg_send_id(class, sel_registerName(c"sharedWorkspace".as_ptr()));
        if workspace.is_null() {
            return reduce_motion_from_macos_flag(false, false);
        }
        let reduce = msg_send_bool(
            workspace,
            sel_registerName(c"accessibilityDisplayShouldReduceMotion".as_ptr()),
        );
        reduce_motion_from_macos_flag(true, reduce != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ENTER_MS, opacity_fade_plays, reduce_motion_from_macos_flag,
        reduce_motion_from_windows_animation,
    };

    #[test]
    fn reduced_motion_skips_opacity_tweens() {
        assert!(opacity_fade_plays(false));
        assert!(!opacity_fade_plays(true));
        assert_eq!(ENTER_MS, 170);
    }

    #[test]
    fn windows_client_area_animation_maps_to_reduce_motion() {
        assert_eq!(
            reduce_motion_from_windows_animation(false, false),
            None,
            "a failed query leaves the preference unset"
        );
        assert_eq!(
            reduce_motion_from_windows_animation(true, true),
            Some(false)
        );
        assert_eq!(
            reduce_motion_from_windows_animation(true, false),
            Some(true)
        );
    }

    #[test]
    fn macos_display_option_maps_to_reduce_motion() {
        assert_eq!(reduce_motion_from_macos_flag(false, true), None);
        assert_eq!(reduce_motion_from_macos_flag(true, false), Some(false));
        assert_eq!(reduce_motion_from_macos_flag(true, true), Some(true));
    }
}
