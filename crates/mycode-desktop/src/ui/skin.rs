//! Quiet panels over a near-solid page.
//!
//! The window background is a very slight wash. Rails, dialogs, and the
//! composer stay opaque enough that a label never disappears, without
//! stacking a new color block for every control.
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    Background, Div, Hsla, InteractiveElement as _, Stateful, Styled as _, div, linear_color_stop,
    linear_gradient, px,
};

/// Page wash. The end stop is already pulled toward the base in the palette,
/// so the column reads as one dark surface.
pub(super) fn ambient(theme: &Theme) -> Background {
    linear_gradient(
        180.,
        linear_color_stop(theme.background, 0.),
        linear_color_stop(theme.status_bar, 1.),
    )
}

/// Title bar and composer strip.
pub(super) fn glass(theme: &Theme) -> Hsla {
    theme.title_bar
}

/// Sidebar and inspector.
pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    theme.sidebar
}

/// Quiet control fill. Nearly transparent so a border can carry the edge.
pub(super) fn frost(theme: &Theme) -> Hsla {
    theme.secondary.opacity(0.28)
}

/// Dialog and form card fill.
pub(super) fn frost_card(theme: &Theme) -> Hsla {
    theme.popover
}

/// Toast fill. Opaque so one line of ink stays readable.
pub(super) fn toast_fill(theme: &Theme) -> Hsla {
    theme.popover
}

/// Hover fill.
pub(super) fn frost_hover(theme: &Theme) -> Hsla {
    theme.secondary_hover
}

/// Selected-row fill. Opaque tint, not a translucent accent.
pub(super) fn frost_accent(theme: &Theme) -> Hsla {
    theme.accent
}

/// Card corner radius.
pub(super) fn radius_card() -> gpui_kit::Pixels {
    px(12.)
}

/// Button and chip corner radius.
pub(super) fn radius_control() -> gpui_kit::Pixels {
    px(10.)
}

/// Primary and secondary actions. Primary is a low-saturation tint with a
/// thin accent edge; secondary is a border only. Hover deepens the fill.
pub(super) fn glass_button(
    id: impl Into<gpui_kit::ElementId>,
    emphasized: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let fill = if emphasized {
        super::desk::primary_fill(theme.accent, theme.primary)
    } else {
        theme.transparent
    };
    let hover = if emphasized {
        super::desk::deepen(fill)
    } else {
        theme.secondary_hover
    };
    let border = if emphasized {
        super::desk::primary_edge(theme.primary)
    } else {
        theme.border
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_2()
        .h(px(32.))
        .px(px(12.))
        .rounded(px(11.))
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_color(theme.foreground)
        .text_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(hover).border_color(border))
}

/// Hairline border.
pub(super) fn glass_border(theme: &Theme) -> Hsla {
    theme.border
}

/// Menu fill. Opaque.
pub(super) fn popover(theme: &Theme) -> Hsla {
    theme.popover
}

/// Scrim drawn over the app behind an open menu layer.
pub(super) fn scrim(theme: &Theme) -> Hsla {
    theme.overlay
}

/// The shared floating-panel recipe.
pub(super) fn popover_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .rounded(radius_card())
        .border_1()
        .border_color(glass_border(theme))
        .bg(popover(theme))
        .text_color(theme.popover_foreground)
}
