//! The Desk colors in dark and light. Spec colors stay opaque; chrome applies
//! its own alpha in `ui/skin` over the window blur. The page is a neutral
//! graphite (dark) or warm-neutral (light) canvas with an indigo accent and a
//! faint accent wash.
//!
//! Colors apply by overriding the resolved [`Theme`] after every
//! `Theme::change`, including the button tokens GPUI actually paints.

use gpui_kit::Hsla;
use gpui_kit::component::theme::{Theme, ThemeMode};

/// Semantic surface, border, text, and status tokens for one appearance mode.
#[derive(Clone, Copy, Debug)]
pub struct SemanticTokens {
    pub bg: Hsla,
    pub wash: Hsla,
    pub surface_1: Hsla,
    pub surface_2: Hsla,
    pub surface_3: Hsla,
    pub border_subtle: Hsla,
    pub border_strong: Hsla,
    pub text_1: Hsla,
    pub text_2: Hsla,
    pub text_3: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub accent_ink: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub info: Hsla,
    pub violet: Hsla,
}

fn hex(value: u32) -> Hsla {
    gpui_kit::rgb(value).into()
}

fn hex_a(value: u32, alpha: f32) -> Hsla {
    let mut color = hex(value);
    color.a = alpha;
    color
}

/// Canonical appearance id. Unknown values fall back to dark.
#[must_use]
pub fn normalize_theme(theme: &str) -> &'static str {
    mycode_config::VALID_THEMES
        .into_iter()
        .find(|id| *id == theme)
        .unwrap_or("dark")
}

/// GPUI mode for a stored appearance id.
#[must_use]
pub fn theme_mode(theme: &str) -> ThemeMode {
    if normalize_theme(theme) == "light" {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    }
}

/// Canonical font-size id. Unknown values fall back to medium.
#[must_use]
pub fn normalize_font_size(id: &str) -> &'static str {
    mycode_config::VALID_FONT_SIZES
        .into_iter()
        .find(|item| *item == id)
        .unwrap_or("m")
}

/// Body size in pixels. `.text_sm()` is 0.875rem, so the window rem is chosen
/// to land `text_sm` on this value.
#[must_use]
pub fn interface_font_px(id: &str) -> f32 {
    match normalize_font_size(id) {
        "s" => 12.,
        "l" => 16.,
        "xl" => 18.,
        _ => 14.,
    }
}

/// Root rem that makes `.text_sm()` equal [`interface_font_px`].
#[must_use]
pub fn interface_rem_px(id: &str) -> f32 {
    interface_font_px(id) / 0.875
}

/// Short label for a font-size id.
#[must_use]
pub fn font_size_label(id: &str) -> &'static str {
    match normalize_font_size(id) {
        "s" => "S",
        "l" => "L",
        "xl" => "XL",
        _ => "M",
    }
}

/// Writes the interface scale onto the theme. The caller also sets the window
/// rem so every `text_sm` / `text_xs` surface follows.
pub fn apply_font_size(theme: &mut Theme, id: &str) {
    let rem = interface_rem_px(id);
    theme.font_size = gpui_kit::px(rem);
    theme.mono_font_size = gpui_kit::px(interface_font_px(id));
}

/// GPUI's virtual family for the operating-system UI font.
pub const SYSTEM_UI_FONT: &str = ".SystemUIFont";

/// Families installed on this machine, cached after the first non-empty list.
///
/// Enumerating fonts is expensive, and the set does not change while the
/// process runs. An empty first answer is not cached: the text system may
/// not have loaded yet.
pub fn installed_font_names(cx: &gpui_kit::App) -> &'static [String] {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    if let Some(names) = NAMES.get() {
        return names;
    }
    let names = cx.text_system().all_font_names();
    if names.is_empty() {
        return &[];
    }
    NAMES.get_or_init(|| names)
}

/// Mono families tool rows and diffs may use, best first.
const MONO_FONT_FACES: &[&str] = &[
    "Cascadia Mono",
    "Cascadia Code",
    "Consolas",
    "SF Mono",
    "Menlo",
    "JetBrains Mono",
    "Fira Code",
    "DejaVu Sans Mono",
];

/// First installed mono family, so tool output and diffs land in a real code
/// font instead of whatever the toolkit default resolves to on this machine.
#[must_use]
pub fn mono_font_family(cx: &gpui_kit::App) -> gpui_kit::SharedString {
    let installed = installed_font_names(cx);
    MONO_FONT_FACES
        .iter()
        .find(|name| installed.iter().any(|have| have == *name))
        .map(|name| (*name).into())
        .unwrap_or_else(|| gpui_kit::SharedString::from("monospace"))
}

/// The family the theme uses for UI chrome: always the OS UI font.
///
/// There is no family picker. Named families mapped the chrome's weights
/// differently per face (a 500 or 600 run landed regular in one face and
/// bold in another), so one screen mixed bold and regular headings.
/// `.SystemUIFont` resolves to a concrete installed family when it can, so
/// later text lookups hit the font cache.
pub fn ui_font_family(cx: &gpui_kit::App) -> gpui_kit::SharedString {
    let installed = installed_font_names(cx);
    resolved_system_family(cx, installed).unwrap_or_else(|| SYSTEM_UI_FONT.into())
}

fn resolved_system_family(
    cx: &gpui_kit::App,
    installed: &[String],
) -> Option<gpui_kit::SharedString> {
    if installed.is_empty() {
        return None;
    }
    let text_system = cx.text_system();
    let resolved = text_system
        .get_font_for_id(text_system.resolve_font(&gpui_kit::font(SYSTEM_UI_FONT)))
        .map(|font| font.family)?;
    if resolved.as_ref() == SYSTEM_UI_FONT {
        return None;
    }
    installed
        .iter()
        .any(|name| name == resolved.as_ref())
        .then_some(resolved)
}

/// A slightly deeper tint for primary hover.
#[must_use]
pub(crate) fn deepen(mut color: Hsla) -> Hsla {
    color.l = (color.l - 0.05).max(0.);
    color
}

/// How far a primary fill moves from the quiet tint toward the bright accent.
///
/// The tint alone sits almost on the card. A short step toward the accent
/// keeps the body soft and still lifts it off that surface.
const PRIMARY_FILL_MIX: f32 = 0.16;

/// Accent-edge alpha on a primary control.
///
/// Composited on a dark card, this clears a 3:1 boundary without painting a
/// solid accent ring.
const PRIMARY_EDGE_ALPHA: f32 = 0.66;

/// Primary body. Light ink stays readable; the fill stays a tint.
#[must_use]
pub(crate) fn primary_fill(tint: Hsla, accent: Hsla) -> Hsla {
    soften(tint, accent, PRIMARY_FILL_MIX)
}

/// Thin accent edge for a primary control.
#[must_use]
pub(crate) fn primary_edge(accent: Hsla) -> Hsla {
    accent.opacity(PRIMARY_EDGE_ALPHA)
}

/// Moves `from` toward `toward` by `amount` (0 keeps `from`, 1 is `toward`).
fn soften(from: Hsla, toward: Hsla, amount: f32) -> Hsla {
    Hsla {
        h: from.h + (toward.h - from.h) * amount,
        s: from.s + (toward.s - from.s) * amount,
        l: from.l + (toward.l - from.l) * amount,
        a: from.a + (toward.a - from.a) * amount,
    }
}

struct Spec {
    bg: u32,
    wash: u32,
    surface: u32,
    card: u32,
    hover: u32,
    ink: u32,
    dim: u32,
    faint: u32,
    line: u32,
    line_strong: u32,
    accent: u32,
    accent_ink: u32,
    tint: u32,
    green: u32,
    amber: u32,
    red: u32,
    info: u32,
    violet: u32,
}

/// Resolves semantic tokens in dark or light mode.
#[must_use]
pub fn semantic_tokens(light: bool) -> SemanticTokens {
    let spec = if light { light_spec() } else { dark_spec() };
    SemanticTokens {
        bg: hex(spec.bg),
        wash: hex(spec.wash),
        surface_1: hex(spec.surface),
        surface_2: hex(spec.card),
        surface_3: hex(spec.hover),
        border_subtle: hex(spec.line),
        border_strong: hex(spec.line_strong),
        text_1: hex(spec.ink),
        text_2: hex(spec.dim),
        text_3: hex(spec.faint),
        accent: hex(spec.accent),
        accent_soft: hex(spec.tint),
        accent_ink: hex(spec.accent_ink),
        success: hex(spec.green),
        warning: hex(spec.amber),
        danger: hex(spec.red),
        info: hex(spec.info),
        violet: hex(spec.violet),
    }
}

fn dark_spec() -> Spec {
    Spec {
        bg: 0x0E0F12,
        wash: 0x11131B,
        surface: 0x15171B,
        card: 0x1C1F24,
        hover: 0x24282E,
        ink: 0xECEDEE,
        dim: 0xA1A4AB,
        faint: 0x6B6F78,
        line: 0x22252B,
        line_strong: 0x2E323B,
        accent: 0x7C8CFF,
        accent_ink: 0x0B0E1A,
        tint: 0x1B2036,
        green: 0x34D399,
        amber: 0xF5B544,
        red: 0xF87171,
        info: 0x60A5FA,
        violet: 0xA78BFA,
    }
}

fn light_spec() -> Spec {
    Spec {
        bg: 0xFAFAF9,
        wash: 0xF6F7FB,
        surface: 0xFFFFFF,
        card: 0xF4F4F3,
        hover: 0xEAEAE8,
        ink: 0x18181B,
        dim: 0x52525B,
        faint: 0x71717A,
        line: 0xE4E4E2,
        line_strong: 0xD4D4D0,
        accent: 0x4353D8,
        accent_ink: 0xFFFFFF,
        tint: 0xE5E8FC,
        green: 0x0F7A4D,
        amber: 0xB45309,
        red: 0xB91C1C,
        info: 0x1D4ED8,
        violet: 0x6D28D9,
    }
}

/// Paints the Desk colors in the theme's current light or dark mode.
pub fn apply(theme: &mut Theme) {
    let tokens = semantic_tokens(!theme.mode.is_dark());
    paint(theme, &tokens);
    sync_controls(theme);
}

fn paint(theme: &mut Theme, tokens: &SemanticTokens) {
    let bg = tokens.bg;
    // Keep the neutral canvas dominant; the accent only tints it.
    let wash = soften(tokens.wash, bg, 0.25);
    let surface = tokens.surface_1;
    let card = tokens.surface_2;
    let hover = tokens.surface_3;
    let ink = tokens.text_1;
    let dim = tokens.text_2;
    let line = tokens.border_subtle;
    let accent = tokens.accent;
    let accent_ink = tokens.accent_ink;
    let tint = tokens.accent_soft;
    let green = tokens.success;
    let amber = tokens.warning;
    let red = tokens.danger;
    let info = tokens.info;
    let violet = tokens.violet;

    theme.radius = super::skin::radius_sm();
    theme.radius_lg = super::skin::radius_md();
    theme.shadow = true;

    theme.background = bg;
    theme.foreground = ink;
    theme.muted = card;
    theme.muted_foreground = dim;
    theme.border = line;
    theme.secondary = card;
    theme.secondary_foreground = ink;
    theme.secondary_hover = hover;
    theme.secondary_active = hover;
    theme.accent = tint;
    theme.accent_foreground = accent;
    theme.caret = accent;
    // Selected text. Opaque fills cover the glyphs; this stays translucent
    // and bright enough to see in both modes.
    theme.selection = accent.opacity(0.35);
    theme.primary = accent;
    theme.primary_foreground = accent_ink;
    theme.primary_hover = accent;
    theme.primary_active = accent;
    theme.link = accent;
    theme.link_hover = accent;
    theme.link_active = accent;
    theme.info = info;
    theme.info_foreground = ink;
    theme.success = green;
    theme.success_foreground = ink;
    theme.warning = amber;
    theme.warning_foreground = ink;
    theme.danger = red;
    theme.danger_foreground = if theme.mode.is_dark() {
        bg
    } else {
        hex(0xFFFFFF)
    };
    theme.input = tokens.border_strong;
    theme.ring = accent;

    theme.sidebar = surface;
    theme.sidebar_foreground = dim;
    theme.sidebar_border = line;
    theme.sidebar_accent = tint;
    theme.sidebar_accent_foreground = ink;
    theme.sidebar_primary = accent;
    theme.sidebar_primary_foreground = accent_ink;

    theme.popover = card;
    theme.popover_foreground = ink;
    theme.title_bar = surface;
    theme.title_bar_border = line;
    // Gradient end. The painted title bar uses `title_bar`, not this field.
    theme.status_bar = wash;
    theme.status_bar_border = line;
    theme.tab_bar = surface;
    theme.tab_active = card;
    theme.tab_active_foreground = ink;
    theme.tab_foreground = dim;
    theme.colors.list = surface;
    theme.colors.list_hover = hover;
    theme.colors.list_even = surface;
    theme.colors.list_head = surface;
    theme.table = surface;
    theme.table_hover = hover;
    theme.table_even = soften(surface, hover, 0.55);
    theme.table_head = card;
    theme.table_head_foreground = ink;
    theme.scrollbar = bg;
    theme.scrollbar_thumb = tokens.border_strong;
    theme.scrollbar_thumb_hover = dim;
    theme.window_border = line;
    theme.overlay = hex_a(0x000000, 0.45);

    theme.green = green;
    theme.green_light = green;
    theme.red = red;
    theme.red_light = red;
    theme.blue = info;
    theme.blue_light = info;
    theme.yellow = amber;
    theme.yellow_light = amber;
    theme.magenta = violet;
    theme.magenta_light = violet;
    theme.cyan = info;
    theme.cyan_light = info;
}

/// `Theme::change` resets button tokens to the stock theme. Copy the
/// Desk colors into both the legacy fields and `tokens`.
fn sync_controls(theme: &mut Theme) {
    let primary = primary_fill(theme.accent, theme.primary);
    let primary_hover = deepen(primary);
    theme.button_primary = primary;
    theme.button_primary_hover = primary_hover;
    theme.button_primary_active = primary_hover;
    theme.button_primary_foreground = theme.foreground;
    theme.tokens.button_primary = primary.into();
    theme.tokens.button_primary_hover = primary_hover.into();
    theme.tokens.button_primary_active = primary_hover.into();
    theme.tokens.button_primary_foreground = theme.foreground.into();

    theme.button = theme.transparent;
    theme.button_hover = theme.secondary_hover;
    theme.button_active = theme.secondary_active;
    theme.button_foreground = theme.foreground;
    theme.tokens.button = theme.transparent.into();
    theme.tokens.button_hover = theme.secondary_hover.into();
    theme.tokens.button_active = theme.secondary_active.into();
    theme.tokens.button_foreground = theme.foreground.into();

    theme.tokens.primary = theme.primary.into();
    theme.tokens.primary_hover = theme.primary_hover.into();
    theme.tokens.primary_active = theme.primary_active.into();
    theme.tokens.primary_foreground = theme.primary_foreground.into();
    theme.tokens.secondary = theme.secondary.into();
    theme.tokens.secondary_foreground = theme.foreground.into();
}

/// Diff ink pulled slightly toward the page foreground so the red and green
/// stay readable without the neon preview colors.
#[must_use]
pub(crate) fn diff_ink(theme: &Theme, added: bool) -> Hsla {
    let base = if added { theme.green } else { theme.red };
    soften(base, theme.foreground, 0.12)
}

/// Soft row wash behind a diff line.
#[must_use]
pub(crate) fn diff_wash(ink: Hsla) -> Hsla {
    ink.opacity(0.14)
}

/// Semantic signal colors. `amber` is a distinct warning hue; `violet` and
/// `cyan` are distinct from the accent.
pub struct Desk {
    pub amber: Hsla,
    pub green: Hsla,
    pub red: Hsla,
    pub violet: Hsla,
    pub faint: Hsla,
    pub screen: Hsla,
    pub screen_dim: Hsla,
}

impl Desk {
    pub fn of(theme: &Theme) -> Self {
        let faint = semantic_tokens(!theme.mode.is_dark()).text_3;
        Self {
            amber: theme.warning,
            green: theme.green,
            red: theme.red,
            violet: theme.magenta,
            faint,
            screen: theme.background,
            screen_dim: theme.muted_foreground,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn font_sizes_land_on_the_body_scale() {
        assert_eq!(super::interface_font_px("s"), 12.);
        assert_eq!(super::interface_font_px("m"), 14.);
        assert_eq!(super::interface_font_px("l"), 16.);
        assert_eq!(super::interface_font_px("xl"), 18.);
        assert_eq!(super::interface_font_px("nope"), 14.);
        assert!((super::interface_rem_px("m") - 16.).abs() < f32::EPSILON);
        assert_eq!(super::normalize_font_size("xl"), "xl");
        assert_eq!(super::font_size_label("s"), "S");
    }

    #[test]
    fn light_mode_keeps_ink_and_descriptions_readable() {
        let spec = super::light_spec();
        let bg = super::hex(spec.bg);
        let ink = super::hex(spec.ink);
        let dim = super::hex(spec.dim);
        let card = super::hex(spec.card);
        assert!(
            contrast(ink, bg) >= 7.0,
            "light ink on the page is {:.2}",
            contrast(ink, bg)
        );
        assert!(
            contrast(dim, bg) >= 4.5,
            "light description text on the page is {:.2}",
            contrast(dim, bg)
        );
        assert!(
            contrast(dim, card) >= 4.5,
            "light description text on a card is {:.2}",
            contrast(dim, card)
        );
        assert_ne!(spec.bg, super::dark_spec().bg);
        assert_ne!(spec.bg, spec.accent, "page and accent must differ");
        assert_eq!(super::normalize_theme("light"), "light");
        assert_eq!(super::normalize_theme("nope"), "dark");
        assert_eq!(
            super::theme_mode("light"),
            gpui_kit::component::theme::ThemeMode::Light
        );
        assert_eq!(
            super::theme_mode("dark"),
            gpui_kit::component::theme::ThemeMode::Dark
        );
    }

    /// Primary controls stay a tint: readable ink, a body that lifts off the
    /// card, and an edge that still meets a 3:1 boundary.
    #[test]
    fn primary_fill_stays_soft_and_readable_in_dark_mode() {
        let spec = super::dark_spec();
        let tint = super::hex(spec.tint);
        let accent = super::hex(spec.accent);
        let ink = super::hex(spec.ink);
        let card = super::hex(spec.card);
        let fill = super::primary_fill(tint, accent);
        let edge = super::primary_edge(accent);
        let text = contrast(ink, fill);
        let body = contrast(fill, card);
        let boundary = contrast(over(edge, card), card);
        assert!(
            text >= 4.5,
            "ink on the primary fill is {text:.2}, want >= 4.5"
        );
        assert!(
            (1.1..=2.2).contains(&body),
            "fill against the card is {body:.2}, want a soft 1.1..=2.2"
        );
        assert!(
            boundary >= 3.0,
            "primary edge against the card is {boundary:.2}, want >= 3"
        );
    }

    #[test]
    fn semantic_tokens_keep_neutral_canvas_and_distinct_accessible_status_colors() {
        let dark = super::semantic_tokens(false);
        let light = super::semantic_tokens(true);
        assert_eq!(dark.bg, super::hex(0x0E0F12));
        assert_eq!(dark.surface_1, super::hex(0x15171B));
        assert_eq!(dark.surface_2, super::hex(0x1C1F24));
        assert_eq!(dark.surface_3, super::hex(0x24282E));
        assert_eq!(light.bg, super::hex(0xFAFAF9));
        assert_eq!(light.surface_1, super::hex(0xFFFFFF));
        assert_eq!(light.surface_2, super::hex(0xF4F4F3));

        assert_ne!(
            dark.warning, dark.accent,
            "dark warning must not collapse onto accent"
        );
        assert_ne!(dark.success, dark.warning);
        assert_ne!(dark.warning, dark.danger);
        assert_ne!(dark.danger, dark.info);
        assert!(
            contrast(dark.text_1, dark.bg) >= 12.0,
            "dark text-1 contrast {:.2}",
            contrast(dark.text_1, dark.bg)
        );
        assert!(
            contrast(dark.text_2, dark.surface_1) >= 4.5,
            "dark text-2 contrast {:.2}",
            contrast(dark.text_2, dark.surface_1)
        );
        for (mode, tokens) in [("dark", dark), ("light", light)] {
            assert!(
                contrast(tokens.success, tokens.surface_1) >= 4.5
                    && contrast(tokens.warning, tokens.surface_1) >= 4.5
                    && contrast(tokens.danger, tokens.surface_1) >= 4.5
                    && contrast(tokens.info, tokens.surface_1) >= 4.5,
                "{mode} semantic status hues must clear 4.5:1 on surface-1"
            );
            assert!(
                contrast(
                    tokens.text_1,
                    over(tokens.warning.opacity(0.14), tokens.surface_1)
                ) >= 4.5,
                "{mode} warning banner text must stay readable"
            );
        }
        assert_ne!(
            light.warning, light.accent,
            "light warning must not collapse onto accent"
        );
    }

    fn contrast(a: gpui_kit::Hsla, b: gpui_kit::Hsla) -> f32 {
        let lighter = luminance(a).max(luminance(b));
        let darker = luminance(a).min(luminance(b));
        (lighter + 0.05) / (darker + 0.05)
    }

    fn over(fg: gpui_kit::Hsla, bg: gpui_kit::Hsla) -> gpui_kit::Hsla {
        let src = fg.to_rgb();
        let dst = bg.to_rgb();
        let mix = |src: f32, dst: f32| src * fg.a + dst * (1. - fg.a);
        gpui_kit::Rgba {
            r: mix(src.r, dst.r),
            g: mix(src.g, dst.g),
            b: mix(src.b, dst.b),
            a: 1.,
        }
        .into()
    }

    fn luminance(color: gpui_kit::Hsla) -> f32 {
        let rgb = color.to_rgb();
        let channel = |value: f32| {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b)
    }
}
