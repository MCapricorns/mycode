//! Short opacity fades for view changes.
//!
//! Layout stays put. Only opacity moves, and the animation caps its frame
//! rate so a settings switch does not repaint the desk on every refresh.
use gpui_kit::{Animation, AnimationExt, ElementId, IntoElement, Styled, ease_out_quint};

const ENTER_MS: u64 = 170;

/// Fades `element` in once. The id restarts the fade when the page changes;
/// the same id leaves a finished fade at full opacity.
pub(super) fn fade_in(id: impl Into<ElementId>, element: impl FadeTarget) -> impl IntoElement {
    element.fade_in(id.into())
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
