//! Shared workbench surfaces, corner sizes, and floating panel elevation.
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    BoxShadow, Div, Hsla, InteractiveElement as _, ParentElement as _, Stateful, Styled as _, div,
    hsla, px, relative,
};

pub(super) fn ambient(theme: &Theme) -> Hsla {
    theme.background
}

pub(super) fn panel_fill(theme: &Theme) -> Hsla {
    theme.popover
}

pub(super) fn glass(theme: &Theme) -> Hsla {
    theme.title_bar
}

pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    theme.sidebar
}

/// Semibold resolves to a real Segoe UI and CJK face on Windows.
pub(crate) const WEIGHT_STRONG: gpui_kit::FontWeight = gpui_kit::FontWeight::SEMIBOLD;

pub(super) fn segment_track(theme: &Theme) -> Hsla {
    theme.sidebar
}

pub(super) fn segment_thumb(theme: &Theme) -> Hsla {
    theme.popover
}

pub(super) fn progress_track(fraction: f32, fill: Hsla, theme: &Theme) -> Div {
    div()
        .h(px(4.))
        .rounded_full()
        .bg(theme.border)
        .overflow_hidden()
        .child(div().h_full().w(relative(fraction)).rounded_full().bg(fill))
}

pub(super) fn modal_scrim(theme: &Theme) -> Hsla {
    hsla(0., 0., 0., if theme.mode.is_dark() { 0.52 } else { 0.28 })
}

pub(super) const SPACE_PX: f32 = 4.;

pub(super) fn space(steps: u32) -> gpui_kit::Pixels {
    px(SPACE_PX * steps as f32)
}

pub(super) fn radius_xs() -> gpui_kit::Pixels {
    px(4.)
}

pub(super) fn radius_sm() -> gpui_kit::Pixels {
    px(6.)
}

pub(super) fn radius_md() -> gpui_kit::Pixels {
    px(8.)
}

pub(super) fn radius_card() -> gpui_kit::Pixels {
    radius_md()
}

pub(super) fn radius_control() -> gpui_kit::Pixels {
    radius_sm()
}

/// A single soft shadow separates floating surfaces from the work area.
pub(super) fn elevation_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let alpha = if theme.mode.is_dark() { 0.26 } else { 0.12 };
    vec![BoxShadow::new(px(0.), px(6.), hsla(0., 0., 0., alpha)).blur_radius(px(18.))]
}

/// Use a filled primary action and a neutral frame for secondary controls.
pub(super) fn glass_button(
    id: impl Into<gpui_kit::ElementId>,
    emphasized: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let (fill, ink, border, hover) = if emphasized {
        (
            theme.primary,
            theme.primary_foreground,
            theme.primary_hover,
            theme.primary_hover,
        )
    } else {
        (
            theme.secondary,
            theme.foreground,
            theme.border,
            theme.secondary_hover,
        )
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(space(2))
        .h(px(32.))
        .px(space(3))
        .rounded(radius_control())
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_color(ink)
        .text_sm()
        .when(emphasized, |this| this.font_weight(WEIGHT_STRONG))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
}

/// Shared visual treatment for square 30px chrome toggle icon buttons.
pub(super) fn apply_active_toggle(el: Stateful<Div>, active: bool, theme: &Theme) -> Stateful<Div> {
    let hover = if active {
        theme.accent
    } else {
        theme.secondary_hover
    };
    el.size(px(30.))
        .rounded(radius_control())
        .border_1()
        .border_color(if active {
            theme.accent_foreground.opacity(0.28)
        } else {
            theme.transparent
        })
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if active {
            theme.accent_foreground
        } else {
            theme.muted_foreground
        })
        .when(active, |this| this.bg(theme.accent))
        .hover(move |this| this.bg(hover))
}

pub(super) fn menu_scrim(theme: &Theme) -> Hsla {
    theme.overlay.opacity(0.55)
}

pub(super) fn popover_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .rounded(radius_card())
        .border_1()
        .border_color(theme.border)
        .bg(panel_fill(theme))
        .shadow(elevation_shadow(theme))
        .text_color(theme.popover_foreground)
}

pub(super) use popover_panel as menu_panel;
