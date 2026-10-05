//! Startup veil: the mark fades in, then the veil fades out onto the desk.
//!
//! The hold keeps the wordmark readable. The fade is opacity only, on a
//! layer that does not relayout the desk underneath. Reduced motion skips
//! the veil entirely.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    Animation, AnimationExt as _, Context, InteractiveElement as _, IntoElement, ParentElement,
    Styled, div, ease_in_out, ease_out_quint, img, px,
};

use super::skin;
use crate::workspace::Workspace;

/// How long the mark is fully covered before the veil starts to lift.
pub(super) const SPLASH_HOLD: std::time::Duration = std::time::Duration::from_millis(620);
/// How long the veil takes to disappear.
pub(super) const SPLASH_FADE: std::time::Duration = std::time::Duration::from_millis(380);

/// Hold plus fade. The dismiss timer waits this long, plus a few frames.
#[must_use]
pub(crate) fn splash_total() -> std::time::Duration {
    SPLASH_HOLD + SPLASH_FADE
}

pub(super) fn render_splash(cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme().clone();
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
        .child(div().w(px(36.)).h(px(2.)).rounded_full().bg(theme.primary))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Harness"),
        )
        .with_animation(
            "startup-splash-mark",
            Animation::new(std::time::Duration::from_millis(420))
                .with_easing(ease_out_quint())
                .with_max_fps(30.),
            |this, delta| this.opacity(delta),
        );
    div()
        .id("startup-splash")
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(skin::glass(&theme))
        .child(mark)
        .with_animations(
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
}
