//! Shared opaque surfaces and controls for the paper-and-ink workspace.
//!
//! Surface roles come from Desk. Only floating menus and dialogs cast a
//! shadow; reading surfaces remain flat and independent of the wallpaper.
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    Background, BoxShadow, Div, Hsla, InteractiveElement as _, Stateful, Styled as _, div, hsla, px,
};

/// The reading surface is a single flat field.
pub(super) fn ambient(theme: &Theme) -> Background {
    theme.background.into()
}

/// Questions share the same opaque surface as other floating panels.
pub(super) fn ask_glass(theme: &Theme) -> Background {
    theme.popover.into()
}

pub(super) fn glass(theme: &Theme) -> Hsla {
    theme.title_bar
}

pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    theme.sidebar
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

/// A selected segment uses a surface step without adding another elevation.
pub(super) fn thumb_shadow(_theme: &Theme) -> Vec<BoxShadow> {
    Vec::new()
}

pub(super) fn modal_scrim(theme: &Theme) -> Hsla {
    hsla(0., 0., 0., if theme.mode.is_dark() { 0.5 } else { 0.28 })
}

pub(super) const SPACE_PX: f32 = 4.;

pub(super) fn space(steps: u32) -> gpui_kit::Pixels {
    px(SPACE_PX * steps as f32)
}

/// Prose and tool output share the same gutter and maximum measure.
pub(super) const PROSE_MAX_PX: f32 = 1200.;

pub(super) fn prose_max() -> gpui_kit::Pixels {
    px(PROSE_MAX_PX)
}

pub(super) fn radius_xs() -> gpui_kit::Pixels {
    px(4.)
}

pub(super) fn radius_sm() -> gpui_kit::Pixels {
    px(8.)
}

pub(super) fn radius_md() -> gpui_kit::Pixels {
    px(16.)
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

/// Elevation belongs only to menus, toasts, and dialogs.
pub(super) fn elevation_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let alpha = if theme.mode.is_dark() { 0.24 } else { 0.08 };
    vec![BoxShadow::new(px(0.), px(6.), hsla(0., 0., 0., alpha)).blur_radius(px(16.))]
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
        (theme.transparent, theme.foreground, border_strong(theme))
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
        .bg(popover(theme))
        .shadow(elevation_shadow(theme))
        .text_color(theme.popover_foreground)
}

pub(super) fn dialog_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    popover_panel(id, theme)
}

pub(super) fn menu_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    popover_panel(id, theme).bg(opaque_menu_fill(theme))
}
