//! Frosted chrome over the platform window blur.
//!
//! GPUI in this tree has no per-element backdrop filter. The blur is the
//! window material (`WindowBackgroundAppearance::Blurred`: macOS visual
//! effect, Windows DWM blur, Wayland KDE blur). These fills are the
//! translucent half: low enough that the material shows, high enough that
//! ink stays readable when a platform cannot blur and a light desktop
//! shows through. The reading column stays closer to solid than the rails.
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    Background, Div, Hsla, InteractiveElement as _, Stateful, Styled as _, div, linear_color_stop,
    linear_gradient, px,
};

/// Sidebar, title bar, settings nav, inspector. Translucent over the blur.
const RAIL_ALPHA: f32 = 0.58;
/// Conversation and settings page. Still readable, with the window blur
/// and the palette gradient showing through.
const PAGE_ALPHA: f32 = 0.82;
/// Menus, dialogs, and the composer. Frosted, not an opaque slab.
const OVERLAY_ALPHA: f32 = 0.74;
/// Form cards on the reading page. A small lift plus translucency, so a
/// card is not the same color as the page under it.
const CARD_ALPHA: f32 = 0.68;

const _: () = {
    assert!(RAIL_ALPHA < PAGE_ALPHA);
    assert!(PAGE_ALPHA < 1.);
    assert!(OVERLAY_ALPHA < 1.);
    assert!(CARD_ALPHA < 1.);
    assert!(RAIL_ALPHA >= 0.5);
};

/// Reading-column wash. Stops stay translucent so a hint of the window
/// material remains; the palette wash is already pulled toward the base.
pub(super) fn ambient(theme: &Theme) -> Background {
    linear_gradient(
        180.,
        linear_color_stop(with_alpha(theme.background, PAGE_ALPHA), 0.),
        linear_color_stop(with_alpha(theme.status_bar, PAGE_ALPHA), 1.),
    )
}

/// Translucent rail fill for the title bar, the settings header, and the
/// preset sign-in code card.
pub(super) fn glass(theme: &Theme) -> Hsla {
    with_alpha(theme.title_bar, RAIL_ALPHA)
}

/// Sidebar, settings nav, and inspector.
pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    with_alpha(theme.sidebar, RAIL_ALPHA)
}

/// Quiet control fill. Nearly transparent so a border can carry the edge.
pub(super) fn frost(theme: &Theme) -> Hsla {
    theme.secondary.opacity(0.28)
}

/// Dialog and form card fill. Lifted off the page so the card edge reads.
pub(super) fn frost_card(theme: &Theme) -> Hsla {
    lifted(theme.popover, 0.04, CARD_ALPHA)
}

/// Toast fill. Same glass as other overlays; the border carries the edge.
pub(super) fn toast_fill(theme: &Theme) -> Hsla {
    lifted(theme.popover, 0.03, OVERLAY_ALPHA)
}

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
        glass_border(theme)
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

/// Panel edge. Stronger than the palette hairline, which sits only a few
/// steps off the page and disappears once a fill is translucent.
pub(super) fn glass_border(theme: &Theme) -> Hsla {
    theme.muted_foreground.opacity(0.42)
}

/// Menu and overlay fill.
pub(super) fn popover(theme: &Theme) -> Hsla {
    lifted(theme.popover, 0.03, OVERLAY_ALPHA)
}

/// Light dimmer behind a menu. The panel is a sibling painted after this,
/// so the scrim does not grey out the menu itself.
pub(super) fn menu_scrim(theme: &Theme) -> Hsla {
    theme.overlay.opacity(0.55)
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

fn with_alpha(mut color: Hsla, alpha: f32) -> Hsla {
    color.a = alpha.clamp(0., 1.);
    color
}

fn lifted(mut color: Hsla, lightness: f32, alpha: f32) -> Hsla {
    color.l = (color.l + lightness).clamp(0., 1.);
    color.a = alpha.clamp(0., 1.);
    color
}
