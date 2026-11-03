use gpui_kit::{Bounds, px, size};

use super::window::{TRAFFIC_LIGHTS, WINDOW_FLOOR, WINDOW_SIZE, fit_window};
use super::*;

#[cfg(feature = "test-support")]
#[gpui_kit::test]
fn startup_shortcut_resolves_the_platform_modifier(cx: &mut gpui_kit::TestAppContext) {
    cx.update(|cx| {
        bind_command_palette_shortcut("secondary-k", cx);
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let binding = keymap
            .bindings()
            .find(|binding| {
                binding
                    .action()
                    .as_any()
                    .is::<crate::ui::ToggleCommandPalette>()
            })
            .expect("command palette shortcut registered");
        let shortcut = if cfg!(target_os = "macos") {
            "cmd-k"
        } else {
            "ctrl-k"
        };
        let keystroke = gpui_kit::Keystroke::parse(shortcut).expect("native shortcut");
        assert_eq!(binding.match_keystrokes(&[keystroke]), Some(false));
    });
}

#[cfg(feature = "test-support")]
#[gpui_kit::test]
fn invalid_shortcut_does_not_abort_startup(cx: &mut gpui_kit::TestAppContext) {
    cx.update(|cx| {
        let before = cx.key_bindings().borrow().bindings().len();
        bind_command_palette_shortcut("invalid-modifier-k", cx);
        assert_eq!(cx.key_bindings().borrow().bindings().len(), before);
        bind_command_palette_shortcut("secondary-k", cx);
        assert_eq!(cx.key_bindings().borrow().bindings().len(), before + 1);
    });
}

#[test]
fn client_chrome_hides_the_system_title_bar() {
    let options = client_window_options(
        Bounds {
            origin: gpui_kit::point(px(0.), px(0.)),
            size: WINDOW_SIZE,
        },
        WINDOW_SIZE,
    );
    assert!(options.app_owns_titlebar_drag);
    #[cfg(target_os = "linux")]
    assert_eq!(
        options.window_decorations,
        Some(gpui_kit::WindowDecorations::Client)
    );
    let titlebar = options.titlebar.expect("custom title bar");
    assert!(titlebar.appears_transparent);
    assert_eq!(titlebar.title.as_ref().map(AsRef::as_ref), Some("MYCode"));
    assert_eq!(titlebar.traffic_light_position, Some(TRAFFIC_LIGHTS));
    assert!(f32::from(TRAFFIC_LIGHTS.x) >= 0.);
    assert!(f32::from(TRAFFIC_LIGHTS.y) >= 0.);
}

#[test]
fn the_default_size_is_fitted_and_the_floor_stays_adjustable() {
    let screen = size(px(1920.), px(1032.));
    let (window, min) = fit_window(Some(screen), None);
    assert_eq!(window, size(px(1440.), px(900.)));
    assert_eq!(min, WINDOW_FLOOR);
    // A saved size smaller than the default is honored, not grown back.
    let (window, _) = fit_window(Some(screen), Some(size(px(1000.), px(700.))));
    assert_eq!(window, size(px(1000.), px(700.)));
    // Below the floor the window stops at the floor; a larger size stays.
    let (window, _) = fit_window(Some(screen), Some(size(px(600.), px(400.))));
    assert_eq!(window, WINDOW_FLOOR);
    let big = size(px(2400.), px(1300.));
    let (window, _) = fit_window(Some(size(px(2560.), px(1440.))), Some(big));
    assert_eq!(window, big);
    // A small screen shrinks the default instead of demanding more room.
    let (window, min) = fit_window(Some(size(px(1280.), px(720.))), None);
    assert_eq!(window, size(px(1216.), px(656.)));
    assert_eq!(min, WINDOW_FLOOR);
    assert_eq!(fit_window(None, None), (WINDOW_SIZE, WINDOW_FLOOR));
}

#[test]
fn a_busy_close_request_only_confirms_inside_its_window() {
    let now = Instant::now();
    assert!(!close_allowed(true, None, now));
    assert!(close_allowed(true, Some(now), now));
    assert!(close_allowed(
        true,
        Some(now - (CLOSE_CONFIRM_WINDOW - Duration::from_secs(1))),
        now
    ));
    assert!(!close_allowed(
        true,
        Some(now - (CLOSE_CONFIRM_WINDOW + Duration::from_secs(1))),
        now
    ));
}

#[test]
fn an_idle_close_request_quits_at_once() {
    let now = Instant::now();
    assert!(close_allowed(false, None, now));
    assert!(close_allowed(
        false,
        Some(now - (CLOSE_CONFIRM_WINDOW + Duration::from_secs(1))),
        now
    ));
}
