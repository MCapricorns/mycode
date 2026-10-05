//! Startup veil: the mark fades in, then the veil fades out onto the desk.
//!
//! The hold keeps the wordmark readable. The fade is opacity only, on a
//! layer that does not relayout the desk underneath. Reduced motion never
//! mounts the veil, so those tweens do not run. A click or Escape drops a
//! playing veil immediately.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, Context, InteractiveElement as _, IntoElement,
    ParentElement, StatefulInteractiveElement as _, Styled, div, ease_in_out, ease_out_quint, img,
    px,
};

use super::motion::opacity_fade_plays;
use super::skin;
use crate::workspace::Workspace;

/// How long the mark is fully covered before the veil starts to lift.
pub(super) const SPLASH_HOLD: std::time::Duration = std::time::Duration::from_millis(620);
/// How long the veil takes to disappear.
pub(super) const SPLASH_FADE: std::time::Duration = std::time::Duration::from_millis(380);
/// A few frames after the fade so the overlay unmounts once it is gone.
const SPLASH_UNMOUNT_SLACK: std::time::Duration = std::time::Duration::from_millis(40);

/// Hold plus fade. The dismiss timer waits this long, plus a few frames.
#[must_use]
pub(crate) fn splash_total() -> std::time::Duration {
    SPLASH_HOLD + SPLASH_FADE
}

/// Startup veil latch. The timer is armed once. Reduced motion dismisses
/// without arming it, including when the preference flips on mid-veil.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SplashGate {
    started: bool,
    dismissed: bool,
}

impl SplashGate {
    pub(crate) fn new() -> Self {
        Self {
            started: false,
            dismissed: false,
        }
    }

    /// Advances one frame. `Some` is the delay before the timed dismiss,
    /// and only the frame that starts the veil returns it.
    pub(crate) fn on_frame(&mut self, reduce_motion: bool) -> Option<std::time::Duration> {
        if self.dismissed {
            return None;
        }
        if reduce_motion {
            self.dismissed = true;
            return None;
        }
        if self.started {
            return None;
        }
        self.started = true;
        Some(splash_total() + SPLASH_UNMOUNT_SLACK)
    }

    /// Drops the veil immediately. False when it was already gone.
    pub(crate) fn dismiss(&mut self) -> bool {
        if self.dismissed {
            return false;
        }
        self.dismissed = true;
        true
    }

    #[must_use]
    pub(crate) fn visible(&self) -> bool {
        !self.dismissed
    }
}

pub(super) fn render_splash(cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let reduce_motion = cx.reduce_motion();
    let mark = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .child(img("brand/icon.ico").size(px(48.)))
        .child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child("MYCode"),
        )
        .child(div().w(px(36.)).h(px(2.)).rounded_full().bg(theme.primary));
    let mark: AnyElement = if opacity_fade_plays(reduce_motion) {
        mark.with_animation(
            "startup-splash-mark",
            Animation::new(std::time::Duration::from_millis(420))
                .with_easing(ease_out_quint())
                .with_max_fps(30.),
            |this, delta| this.opacity(delta),
        )
        .into_any_element()
    } else {
        mark.into_any_element()
    };
    let veil = div()
        .id("startup-splash")
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(skin::glass(&theme))
        .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.dismiss_splash(cx);
        }))
        .child(mark);
    if opacity_fade_plays(reduce_motion) {
        veil.with_animations(
            "startup-splash-veil",
            vec![
                Animation::new(SPLASH_HOLD).with_max_fps(30.),
                Animation::new(SPLASH_FADE)
                    .with_easing(ease_in_out)
                    .with_max_fps(30.),
            ],
            |this, index, delta| {
                if index == 0 {
                    this
                } else {
                    this.opacity(1.0 - delta)
                }
            },
        )
        .into_any_element()
    } else {
        veil.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{SPLASH_FADE, SPLASH_HOLD, SPLASH_UNMOUNT_SLACK, SplashGate, splash_total};

    #[test]
    fn timed_dismiss_waits_for_hold_fade_and_unmount_slack() {
        let mut gate = SplashGate::new();
        let wait = gate.on_frame(false).expect("veil starts");
        assert_eq!(wait, splash_total() + SPLASH_UNMOUNT_SLACK);
        assert_eq!(splash_total(), SPLASH_HOLD + SPLASH_FADE);
        assert!(gate.visible());
        assert!(gate.on_frame(false).is_none(), "the timer arms once");
    }

    #[test]
    fn click_or_escape_dismisses_before_the_timer() {
        let mut gate = SplashGate::new();
        assert!(gate.on_frame(false).is_some());
        assert!(gate.dismiss());
        assert!(!gate.visible());
        assert!(!gate.dismiss());
        assert!(gate.on_frame(false).is_none());
    }

    #[test]
    fn reduced_motion_never_mounts_and_drops_a_playing_veil() {
        let mut skipped = SplashGate::new();
        assert!(skipped.on_frame(true).is_none());
        assert!(!skipped.visible());
        assert!(!skipped.dismiss());

        let mut playing = SplashGate::new();
        assert!(playing.on_frame(false).is_some());
        assert!(playing.on_frame(true).is_none());
        assert!(!playing.visible());
    }
}
