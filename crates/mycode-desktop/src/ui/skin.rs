//! Frosted chrome over the platform window blur.
//!
//! GPUI in this tree has no per-element backdrop filter. The blur is the
//! window material (`WindowBackgroundAppearance::Blurred`: macOS visual
//! effect, Windows DWM blur). These fills are the
//! translucent half: low enough that the material shows, high enough that
//! ink stays readable when a platform cannot blur and a light desktop
//! shows through. The reading column stays closer to solid than the rails.
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    Background, BoxShadow, Div, Hsla, InteractiveElement as _, Stateful, Styled as _, div, hsla,
    linear_color_stop, linear_gradient, px,
};

/// Sidebar, title bar, settings nav, inspector. Translucent over the blur.
const RAIL_ALPHA: f32 = 0.62;
/// Conversation and settings page. Still readable, with the window blur
/// and the accent wash showing through.
const PAGE_ALPHA: f32 = 0.88;
/// Menus, dialogs, and the composer. Frosted, not an opaque slab.
const OVERLAY_ALPHA: f32 = 0.82;
/// Form cards on the reading page. A small lift plus translucency, so a
/// card is not the same color as the page under it.
const CARD_ALPHA: f32 = 0.76;

const _: () = {
    assert!(RAIL_ALPHA < PAGE_ALPHA);
    assert!(PAGE_ALPHA < 1.);
    assert!(OVERLAY_ALPHA < 1.);
    assert!(CARD_ALPHA < 1.);
    assert!(RAIL_ALPHA >= 0.5);
};

/// Reading-column wash. Stops stay translucent so a hint of the window
/// material remains; the accent wash is already pulled toward the base.
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

/// Toast fill. Raised surface-2 so notifications read clearly over the page.
pub(super) fn toast_fill(theme: &Theme) -> Hsla {
    lifted(theme.popover, 0.02, 0.94)
}

pub(super) fn frost_hover(theme: &Theme) -> Hsla {
    theme.secondary_hover
}

/// Selected-row fill. Opaque tint, not a translucent accent.
pub(super) fn frost_accent(theme: &Theme) -> Hsla {
    theme.accent
}

/// The one emphasis weight for chrome: page, card, and dialog titles, list
/// item names, and table heads. Everything else is regular.
///
/// No 500 (medium) anywhere. Segoe UI and Microsoft YaHei UI ship no 500
/// face, so DirectWrite paints a 500 run regular on Windows while macOS
/// paints it medium, and half the emphasis vanished on one platform. 600 is
/// a real Segoe UI face and resolves to the bold CJK face, so one weight
/// reads as emphasis everywhere.
pub(crate) const WEIGHT_STRONG: gpui_kit::FontWeight = gpui_kit::FontWeight::SEMIBOLD;

/// Recessed track behind a segmented control, darker than the card in both
/// modes so the raised thumb reads as the pick.
pub(super) fn segment_track(theme: &Theme) -> Hsla {
    if theme.mode.is_dark() {
        with_alpha(theme.background, 0.6)
    } else {
        theme.secondary_hover
    }
}

/// Raised thumb for the picked segment.
pub(super) fn segment_thumb(theme: &Theme) -> Hsla {
    if theme.mode.is_dark() {
        lifted(theme.popover, 0.1, 1.)
    } else {
        theme.sidebar
    }
}

/// Hairline lift under a segmented-control thumb.
pub(super) fn thumb_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let alpha = if theme.mode.is_dark() { 0.4 } else { 0.12 };
    vec![BoxShadow::new(px(0.), px(1.), hsla(0., 0., 0., alpha)).blur_radius(px(2.))]
}

/// Scrim behind every modal dialog. The update dialog, the project picker,
/// and the command palette dim the window the same way.
pub(super) fn modal_scrim(theme: &Theme) -> Hsla {
    if theme.mode.is_dark() {
        hsla(0., 0., 0., 0.5)
    } else {
        hsla(0., 0., 0., 0.28)
    }
}

/// Base step of the spacing scale. Padding, gaps, and radii are multiples of this.
pub(super) const SPACE_PX: f32 = 4.;

/// `steps` times [`SPACE_PX`].
pub(super) fn space(steps: u32) -> gpui_kit::Pixels {
    px(SPACE_PX * steps as f32)
}

/// Left-aligned reading cap. The column shares the tool-card gutter and only
/// stops growing on an ultrawide pane, so it is not a centered 750px measure.
pub(super) const PROSE_MAX_PX: f32 = 1200.;

pub(super) fn prose_max() -> gpui_kit::Pixels {
    px(PROSE_MAX_PX)
}

/// 6px. Dots, swatch corners, and compact chips.
pub(super) fn radius_xs() -> gpui_kit::Pixels {
    px(6.)
}

/// 10px. Controls, rows, and compact chips.
pub(super) fn radius_sm() -> gpui_kit::Pixels {
    px(10.)
}

/// 14px. Cards, menus, and panels.
pub(super) fn radius_md() -> gpui_kit::Pixels {
    px(14.)
}

/// Card corner radius.
pub(super) fn radius_card() -> gpui_kit::Pixels {
    radius_md()
}

/// Button and chip corner radius.
pub(super) fn radius_control() -> gpui_kit::Pixels {
    radius_sm()
}

/// Subtle surface border (`rgba(255,255,255,0.07)` dark, `rgba(0,0,0,0.08)` light).
pub(super) fn border_subtle(theme: &Theme) -> Hsla {
    if theme.mode.is_dark() {
        hsla(0., 0., 1., 0.07)
    } else {
        hsla(0., 0., 0., 0.08)
    }
}

/// Stronger interactive or elevated border (`rgba(255,255,255,0.12)` dark, `rgba(0,0,0,0.14)` light).
pub(super) fn border_strong(theme: &Theme) -> Hsla {
    if theme.mode.is_dark() {
        hsla(0., 0., 1., 0.12)
    } else {
        hsla(0., 0., 0., 0.14)
    }
}

/// Soft elevation shadow for floating menus, composer, toasts, and dialogs.
pub(super) fn elevation_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let alpha = if theme.mode.is_dark() { 0.35 } else { 0.08 };
    let ink = hsla(0., 0., 0., alpha);
    vec![
        BoxShadow::new(px(0.), px(8.), ink)
            .blur_radius(px(12.))
            .spread_radius(px(-2.)),
        BoxShadow::new(px(0.), px(2.), hsla(0., 0., 0., alpha * 0.6))
            .blur_radius(px(4.))
            .spread_radius(px(-1.)),
    ]
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
        border_strong(theme)
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
        .text_color(theme.foreground)
        .text_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(hover).border_color(border))
}

/// Panel edge. Uses the subtle semantic border so surfaces layer by elevation
/// rather than heavy lines.
pub(super) fn glass_border(theme: &Theme) -> Hsla {
    border_subtle(theme)
}

/// Menu and overlay fill. Translucent so the page shows through lightly.
pub(super) fn popover(theme: &Theme) -> Hsla {
    lifted(theme.popover, 0.03, OVERLAY_ALPHA)
}

/// Thinking-menu fill. Fully opaque so a long transcript does not show through.
const MENU_FILL_ALPHA: f32 = 1.;

const _: () = {
    assert!(MENU_FILL_ALPHA == 1.);
};

pub(super) fn opaque_menu_fill(theme: &Theme) -> Hsla {
    let mut color = theme.popover;
    color.a = MENU_FILL_ALPHA;
    color
}

/// Light dimmer behind a menu. The panel is a sibling painted after this,
/// so the scrim does not grey out the menu itself.
pub(super) fn menu_scrim(theme: &Theme) -> Hsla {
    theme.overlay.opacity(0.55)
}

/// The shared floating-panel recipe.
pub(super) fn popover_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .rounded(radius_card())
        .border_1()
        .border_color(border_strong(theme))
        .bg(popover(theme))
        .shadow(elevation_shadow(theme))
        .text_color(theme.popover_foreground)
}

/// Modal dialogs (update, ask, project picker). Opaque: over the dimmed
/// window a translucent card picks up the scrim and reads grey.
pub(super) fn dialog_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    popover_panel(id, theme).bg(lifted(theme.popover, 0.03, 1.))
}

/// Thinking and model menus. Opaque so the transcript does not show through.
pub(super) fn menu_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    popover_panel(id, theme).bg(opaque_menu_fill(theme))
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
