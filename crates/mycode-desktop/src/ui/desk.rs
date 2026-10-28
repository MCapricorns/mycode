//! Paper, ink, and red accents for the MYCode workspace.
//!
//! The palette takes Cindy's paper and charcoal surfaces as a reference,
//! with quieter reading ink and accessible red actions in both modes.
//! Every surface is opaque so its contrast is independent of the desktop.
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
    /// Assistant prose uses the same warm neutral family as the chrome.
    pub reply: Hsla,
    /// Reasoning stays quieter than the reply without a competing hue.
    pub thinking: Hsla,
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

/// GPUI mode for a stored appearance id.
#[must_use]
pub fn theme_mode(theme: &str) -> ThemeMode {
    if mycode_frontend::normalize_theme(theme) == "light" {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    }
}

pub use mycode_frontend::normalize_font_size;

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
    reply: u32,
    thinking: u32,
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
        reply: hex(spec.reply),
        thinking: hex(spec.thinking),
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
        bg: 0x141316,
        wash: 0x141316,
        surface: 0x1B1A1E,
        card: 0x242328,
        hover: 0x302E33,
        ink: 0xF3EDE2,
        dim: 0xB5B0AC,
        faint: 0xA39D99,
        reply: 0xE9E3D9,
        thinking: 0xB5B0AC,
        line: 0x302E33,
        line_strong: 0x514B50,
        accent: 0xF46A6C,
        accent_ink: 0x1E1115,
        tint: 0x392326,
        green: 0x82C6A4,
        amber: 0xE8BD77,
        red: 0xF38B94,
        info: 0xA9C6E8,
        violet: 0xB5B0AC,
    }
}

fn light_spec() -> Spec {
    Spec {
        bg: 0xFAF6EE,
        wash: 0xFAF6EE,
        surface: 0xF3EDE2,
        card: 0xFFFCF7,
        hover: 0xEAE2D6,
        ink: 0x14131B,
        dim: 0x5D5B66,
        faint: 0x716A6B,
        reply: 0x29262B,
        thinking: 0x666068,
        line: 0xE3DCCF,
        line_strong: 0xB6ACA3,
        accent: 0xC72E38,
        accent_ink: 0xFFFCF7,
        tint: 0xF9E4E1,
        green: 0x256C50,
        amber: 0x85560F,
        red: 0xA62642,
        info: 0x365E8A,
        violet: 0x666068,
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
    theme.sidebar_accent = hover;
    theme.sidebar_accent_foreground = ink;
    theme.sidebar_primary = accent;
    theme.sidebar_primary_foreground = accent_ink;

    theme.popover = card;
    theme.popover_foreground = ink;
    theme.title_bar = surface;
    theme.title_bar_border = line;
    // Status bars share the reading surface.
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
    theme.table_even = surface;
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
    let primary = theme.primary;
    let primary_hover = deepen(primary);
    theme.button_primary = primary;
    theme.button_primary_hover = primary_hover;
    theme.button_primary_active = primary_hover;
    theme.button_primary_foreground = theme.primary_foreground;
    theme.tokens.button_primary = primary.into();
    theme.tokens.button_primary_hover = primary_hover.into();
    theme.tokens.button_primary_active = primary_hover.into();
    theme.tokens.button_primary_foreground = theme.primary_foreground.into();

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
    pub reply: Hsla,
    pub thinking: Hsla,
    pub screen: Hsla,
    pub screen_dim: Hsla,
}

impl Desk {
    pub fn of(theme: &Theme) -> Self {
        let tokens = semantic_tokens(!theme.mode.is_dark());
        Self {
            amber: theme.warning,
            green: theme.green,
            red: theme.red,
            violet: theme.magenta,
            faint: tokens.text_3,
            reply: tokens.reply,
            thinking: tokens.thinking,
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
        assert_eq!(
            super::theme_mode("light"),
            gpui_kit::component::theme::ThemeMode::Light
        );
        assert_eq!(
            super::theme_mode("dark"),
            gpui_kit::component::theme::ThemeMode::Dark
        );
    }

    /// Red actions remain legible at rest and on hover in both appearances.
    #[test]
    fn primary_actions_have_accessible_ink_and_boundaries() {
        for light in [false, true] {
            let tokens = super::semantic_tokens(light);
            for fill in [tokens.accent, super::deepen(tokens.accent)] {
                assert!(contrast(tokens.accent_ink, fill) >= 4.5);
                assert!(contrast(fill, tokens.surface_2) >= 3.0);
            }
        }
    }

    #[test]
    fn semantic_tokens_keep_neutral_canvas_and_distinct_accessible_status_colors() {
        let dark = super::semantic_tokens(false);
        let light = super::semantic_tokens(true);
        assert_eq!(dark.bg, super::hex(0x141316));
        assert_eq!(dark.surface_1, super::hex(0x1B1A1E));
        assert_eq!(dark.surface_2, super::hex(0x242328));
        assert_eq!(dark.surface_3, super::hex(0x302E33));
        assert_eq!(light.bg, super::hex(0xFAF6EE));
        assert_eq!(light.surface_1, super::hex(0xF3EDE2));
        assert_eq!(light.surface_2, super::hex(0xFFFCF7));

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

    /// Reading ink stays neutral and legible; reasoning is one step quieter.
    #[test]
    fn reading_inks_are_readable_and_ordered() {
        for (mode, tokens) in [
            ("dark", super::semantic_tokens(false)),
            ("light", super::semantic_tokens(true)),
        ] {
            let reply = contrast(tokens.reply, tokens.bg);
            let thinking = contrast(tokens.thinking, tokens.bg);
            assert!(reply >= 7.0, "{mode} reply ink is {reply:.2}, want >= 7");
            assert!(
                thinking >= 4.5,
                "{mode} thinking ink is {thinking:.2}, want >= 4.5"
            );
            assert!(thinking < reply, "{mode} thinking must be quieter");
            assert_ne!(tokens.reply, tokens.text_1);
        }
    }

    /// Code tokens hold 4.5:1 on the code-block fill (`muted` is the card);
    /// comments may drop to 3.5:1 because they are meant to recede.
    #[test]
    fn syntax_colors_stay_readable_on_the_code_fill() {
        use crate::ui::syntax::{KINDS, color};
        use mycode_frontend::highlight::TokenKind;
        for (mode, light) in [("dark", false), ("light", true)] {
            let fill = super::semantic_tokens(light).surface_2;
            for kind in KINDS {
                let ratio = contrast(color(kind, light), fill);
                let floor = if kind == TokenKind::Comment { 3.5 } else { 4.5 };
                assert!(
                    ratio >= floor,
                    "{mode} {kind:?} on the code fill is {ratio:.2}, want >= {floor}"
                );
            }
        }
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
