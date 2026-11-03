//! Layered paper surfaces, pixel-scale geometry, and translucent panel elevation.
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    Background, BoxShadow, Div, Hsla, InteractiveElement as _, Stateful, Styled as _, div, hsla,
    linear_color_stop, linear_gradient, px,
};

/// A subdued diagonal tint lets the native window material show through.
pub(super) fn ambient(theme: &Theme) -> Background {
    linear_gradient(
        135.,
        linear_color_stop(theme.background.opacity(0.94), 0.),
        linear_color_stop(
            super::desk::soften(theme.background, theme.primary, 0.07).opacity(0.92),
            1.,
        ),
    )
}

/// Floating surfaces stay dense enough to keep underlying text from bleeding through.
pub(super) fn panel_fill(theme: &Theme) -> Background {
    linear_gradient(
        155.,
        linear_color_stop(theme.popover.opacity(0.99), 0.),
        linear_color_stop(
            super::desk::soften(theme.popover, theme.primary, 0.04).opacity(0.97),
            1.,
        ),
    )
}

pub(super) fn ask_glass(theme: &Theme) -> Background {
    panel_fill(theme)
}

pub(super) fn glass(theme: &Theme) -> Hsla {
    theme.title_bar.opacity(0.82)
}

pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    theme.sidebar.opacity(0.76)
}

pub(super) fn frost(theme: &Theme) -> Hsla {
    theme.secondary
}

pub(super) fn frost_card(theme: &Theme) -> Hsla {
    theme.popover
}

pub(super) fn toast_fill(theme: &Theme) -> Hsla {
    theme.popover
}

pub(super) fn frost_hover(theme: &Theme) -> Hsla {
    theme.secondary_hover
}

/// Pale accent reserved for action choices, not navigation.
pub(super) fn frost_accent(theme: &Theme) -> Hsla {
    theme.accent
}

/// Semibold resolves to a real Segoe UI and CJK face on Windows.
pub(crate) const WEIGHT_STRONG: gpui_kit::FontWeight = gpui_kit::FontWeight::SEMIBOLD;

pub(super) fn segment_track(theme: &Theme) -> Hsla {
    theme.sidebar
}

pub(super) fn segment_thumb(theme: &Theme) -> Hsla {
    theme.popover
}

pub(super) fn modal_scrim(theme: &Theme) -> Hsla {
    hsla(0., 0., 0., if theme.mode.is_dark() { 0.5 } else { 0.28 })
}

pub(super) const SPACE_PX: f32 = 4.;

pub(super) fn space(steps: u32) -> gpui_kit::Pixels {
    px(SPACE_PX * steps as f32)
}

pub(super) fn radius_xs() -> gpui_kit::Pixels {
    px(4.)
}

pub(super) fn radius_sm() -> gpui_kit::Pixels {
    px(8.)
}

pub(super) fn radius_md() -> gpui_kit::Pixels {
    px(12.)
}

pub(super) fn radius_card() -> gpui_kit::Pixels {
    radius_md()
}

pub(super) fn radius_control() -> gpui_kit::Pixels {
    radius_sm()
}

pub(super) fn border_subtle(theme: &Theme) -> Hsla {
    theme.border
}

pub(super) fn border_strong(theme: &Theme) -> Hsla {
    theme.input
}

/// A short contact shadow and a soft outer shadow separate floating surfaces.
pub(super) fn elevation_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let alpha = if theme.mode.is_dark() { 0.28 } else { 0.10 };
    vec![
        BoxShadow::new(px(0.), px(3.), hsla(0., 0., 0., alpha * 0.75)).blur_radius(px(2.)),
        BoxShadow::new(px(0.), px(12.), hsla(0., 0., 0., alpha)).blur_radius(px(24.)),
    ]
}

/// One filled primary action; secondary controls stay neutral.
pub(super) fn glass_button(
    id: impl Into<gpui_kit::ElementId>,
    emphasized: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let (fill, ink, border) = if emphasized {
        (theme.primary, theme.primary_foreground, theme.primary)
    } else {
        (theme.transparent, theme.foreground, border_subtle(theme))
    };
    let hover = if emphasized {
        super::desk::deepen(fill)
    } else {
        theme.secondary_hover
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
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
}

pub(super) fn glass_border(theme: &Theme) -> Hsla {
    border_subtle(theme)
}

pub(super) fn popover(theme: &Theme) -> Hsla {
    theme.popover
}

pub(super) fn opaque_menu_fill(theme: &Theme) -> Hsla {
    theme.popover
}

pub(super) fn menu_scrim(theme: &Theme) -> Hsla {
    theme.overlay.opacity(0.55)
}

pub(super) fn popover_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .rounded(radius_card())
        .border_1()
        .border_color(border_subtle(theme))
        .bg(panel_fill(theme))
        .shadow(elevation_shadow(theme))
        .text_color(theme.popover_foreground)
}

pub(super) fn dialog_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    popover_panel(id, theme)
}

pub(super) fn menu_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    popover_panel(id, theme).bg(opaque_menu_fill(theme))
}
