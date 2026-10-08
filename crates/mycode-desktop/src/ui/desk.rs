//! Dark palettes. Spec colors stay opaque; chrome applies its own alpha in
//! `ui/skin` over the window blur. The page is a near-solid dark base with
//! only a hint of the palette wash, so the conversation is not sitting on
//! a loud gradient.
//!
//! Colors apply by overriding the resolved [`Theme`] after every
//! `Theme::change`, including the button tokens GPUI actually paints.

use gpui_kit::Hsla;
use gpui_kit::component::theme::Theme;

fn hex(value: u32) -> Hsla {
    gpui_kit::rgb(value).into()
}

fn hex_a(value: u32, alpha: f32) -> Hsla {
    let mut color = hex(value);
    color.a = alpha;
    color
}

/// Palette ids the settings page offers, in display order.
///
/// Must stay identical to `mycode_config::VALID_PALETTES`.
pub const PALETTES: [&str; 6] = ["slate", "ocean", "forest", "dusk", "ember", "aurora"];

/// Canonical palette id. Unknown values fall back to slate.
#[must_use]
pub fn normalize_palette(palette: &str) -> &'static str {
    PALETTES
        .into_iter()
        .find(|id| *id == palette)
        .unwrap_or("slate")
}

/// Short label for a palette id.
#[must_use]
pub fn palette_label(palette: &str) -> &'static str {
    let t = crate::i18n::t;
    match normalize_palette(palette) {
        "ocean" => t("Ocean", "海洋"),
        "forest" => t("Forest", "森林"),
        "dusk" => t("Dusk", "暮色"),
        "ember" => t("Ember", "余烬"),
        "aurora" => t("Aurora", "极光"),
        _ => t("Slate", "石板灰"),
    }
}

/// Page base and accent for one settings swatch.
///
/// Both colors come from the same [`Spec`] [`apply_palette`] paints. The
/// base is the page background; the accent is the corner chip and the
/// selected ring.
#[derive(Clone, Copy, Debug)]
pub struct PaletteSwatch {
    pub base: Hsla,
    pub accent: Hsla,
}

/// Preview colors for one palette id. Unknown ids use the slate spec.
#[must_use]
pub fn palette_swatch_colors(palette: &str) -> PaletteSwatch {
    let spec = spec_for(normalize_palette(palette));
    PaletteSwatch {
        base: hex(spec.bg),
        accent: hex(spec.accent),
    }
}

/// Interface font-size ids. Must stay identical to `mycode_config::VALID_FONT_SIZES`.
pub const FONT_SIZES: [&str; 4] = ["s", "m", "l", "xl"];

/// Canonical font-size id. Unknown values fall back to medium.
#[must_use]
pub fn normalize_font_size(id: &str) -> &'static str {
    FONT_SIZES
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
        "l" => 14.,
        "xl" => 16.,
        _ => 13.,
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

/// Swatches on one full row of the settings palette.
///
/// Six palettes fit one row of the settings card.
pub const PALETTE_COLUMNS: u16 = 6;

/// Fixed width of one palette cell, in pixels.
///
/// The cell is a flex item, not a grid track. A percentage width or a grid
/// row (including max-content tracks) still grew into the settings scrollport
/// and left a void under the swatches.
pub const PALETTE_CELL_PX: f32 = 52.;

/// Gap between palette cells, in pixels. Fixed so a font-size change cannot
/// push the seventh cell onto the next line.
pub const PALETTE_GAP_PX: f32 = 6.;

/// Side length of the color square inside a palette cell, in pixels.
pub const PALETTE_SWATCH_PX: f32 = 28.;

/// Width of one full palette row: six fixed cells and the gaps between them.
#[must_use]
pub fn palette_row_max_px() -> f32 {
    let columns = f32::from(PALETTE_COLUMNS.max(1));
    columns * PALETTE_CELL_PX + (columns - 1.) * PALETTE_GAP_PX
}

/// GPUI's virtual family for the operating-system UI font.
pub const SYSTEM_UI_FONT: &str = ".SystemUIFont";

struct FontFace {
    /// Value stored in `appearance.fontFamily`.
    id: &'static str,
    /// Installed family names that satisfy this choice, preferred paint first.
    faces: &'static [&'static str],
}

/// Named faces the General page can offer. Ids match
/// `mycode_config::VALID_FONT_FAMILIES`. A face is shown only when one of
/// `faces` is installed, so a family this OS cannot load is skipped.
const FONT_FACES: &[FontFace] = &[
    FontFace {
        id: "Inter",
        faces: &["Inter", "Inter Variable"],
    },
    FontFace {
        id: "Segoe UI",
        faces: &["Segoe UI", "Segoe UI Variable"],
    },
    FontFace {
        id: "PingFang",
        faces: &["PingFang SC", "PingFang TC", "PingFang HK", "PingFang"],
    },
    FontFace {
        id: "Noto Sans",
        faces: &["Noto Sans"],
    },
];

/// Canonical stored id. Empty and `"system"` are the OS UI font.
#[must_use]
pub fn normalize_font_family(value: &str) -> Option<&'static str> {
    mycode_config::canonical_font_family(value)
}

/// Label for a stored font-family id. Font names stay as proper nouns.
#[must_use]
pub fn font_family_label(id: &str) -> &'static str {
    match normalize_font_family(id) {
        Some("Inter") => "Inter",
        Some("Segoe UI") => "Segoe UI",
        Some("PingFang") => "PingFang",
        Some("Noto Sans") => "Noto Sans",
        _ => crate::i18n::t("System", "系统"),
    }
}

/// Stored ids the dropdown should list: System, then each named face this
/// machine can resolve, in catalog order.
#[must_use]
pub fn available_font_family_ids(installed: &[String]) -> Vec<&'static str> {
    let mut ids = vec![mycode_config::SYSTEM_FONT_FAMILY];
    for face in FONT_FACES {
        if face_installed(face, installed) {
            ids.push(face.id);
        }
    }
    ids
}

/// Family name GPUI should paint for a stored id.
///
/// `"system"` and an unknown or missing face stay on `.SystemUIFont` so a
/// settings file from another computer cannot ask for a family that is not
/// installed. The stored id is left unchanged.
#[must_use]
pub fn paint_font_family(stored: &str, installed: &[String]) -> &'static str {
    let Some(id) = normalize_font_family(stored) else {
        return SYSTEM_UI_FONT;
    };
    if id == mycode_config::SYSTEM_FONT_FAMILY {
        return SYSTEM_UI_FONT;
    }
    FONT_FACES
        .iter()
        .find(|face| face.id == id)
        .and_then(|face| {
            face.faces
                .iter()
                .copied()
                .find(|name| installed.iter().any(|have| have == name))
        })
        .unwrap_or(SYSTEM_UI_FONT)
}

fn face_installed(face: &FontFace, installed: &[String]) -> bool {
    face.faces
        .iter()
        .any(|name| installed.iter().any(|have| have == name))
}

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

/// The family the theme should use for UI chrome.
///
/// System resolves through GPUI's `.SystemUIFont` and, when that lands on a
/// concrete installed family, names that family so later text lookups hit the
/// font cache. The settings document is not rewritten with that concrete name.
pub fn ui_font_family(stored: &str, cx: &gpui_kit::App) -> gpui_kit::SharedString {
    let installed = installed_font_names(cx);
    let paint = paint_font_family(stored, installed);
    if paint == SYSTEM_UI_FONT
        && let Some(resolved) = resolved_system_family(cx, installed)
    {
        return resolved;
    }
    paint.into()
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
/// Composited on a dark card, this clears a 3:1 boundary on every palette
/// without painting a solid accent ring.
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
    line: u32,
    accent: u32,
    accent_ink: u32,
    tint: u32,
    green: u32,
    red: u32,
    info: u32,
}

fn spec_for(palette: &str) -> Spec {
    match palette {
        "ocean" => Spec {
            bg: 0x071428,
            wash: 0x0A3A6E,
            surface: 0x0C1E38,
            card: 0x12304C,
            hover: 0x1A4468,
            ink: 0xE7F2FF,
            dim: 0x8EB4D8,
            line: 0x1E4E78,
            accent: 0x4DA3FF,
            accent_ink: 0x041018,
            tint: 0x12385C,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "forest" => Spec {
            bg: 0x0A1C12,
            wash: 0x146B32,
            surface: 0x102818,
            card: 0x163822,
            hover: 0x1E4C2E,
            ink: 0xE7F8EC,
            dim: 0x8FCB9E,
            line: 0x1E5A34,
            accent: 0x3DDC7A,
            accent_ink: 0x04140A,
            tint: 0x145C30,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "dusk" => Spec {
            bg: 0x140A22,
            wash: 0x4A1878,
            surface: 0x1E1030,
            card: 0x2A1844,
            hover: 0x3C2460,
            ink: 0xF4E9FF,
            dim: 0xC4A6E4,
            line: 0x4C2878,
            accent: 0xC084FC,
            accent_ink: 0x140820,
            tint: 0x3A1868,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "sand" => Spec {
            bg: 0x1A1714,
            wash: 0x2A2218,
            surface: 0x221E1A,
            card: 0x2C2722,
            hover: 0x3A332C,
            ink: 0xF3EDE4,
            dim: 0xC4B8AA,
            line: 0x443C34,
            accent: 0xE0B15A,
            accent_ink: 0x1C1408,
            tint: 0x3A3020,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "rose" => Spec {
            bg: 0x1C1418,
            wash: 0x3A2230,
            surface: 0x26181E,
            card: 0x322028,
            hover: 0x422830,
            ink: 0xF8E8EE,
            dim: 0xC4A8B4,
            line: 0x4A3038,
            accent: 0xE48AA8,
            accent_ink: 0x2A1018,
            tint: 0x3A2430,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "ink" => Spec {
            bg: 0x10141C,
            wash: 0x243044,
            surface: 0x181E28,
            card: 0x202836,
            hover: 0x2A3448,
            ink: 0xE8EEF8,
            dim: 0xA8B4C8,
            line: 0x344058,
            accent: 0x8EB4FF,
            accent_ink: 0x10141C,
            tint: 0x243050,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "moss" => Spec {
            bg: 0x121814,
            wash: 0x243028,
            surface: 0x1A221C,
            card: 0x222C24,
            hover: 0x2C3A30,
            ink: 0xE8F2E6,
            dim: 0xA8BCA8,
            line: 0x344438,
            accent: 0x8FBF6A,
            accent_ink: 0x12180E,
            tint: 0x243428,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "ember" => Spec {
            bg: 0x241008,
            wash: 0x8A3410,
            surface: 0x321608,
            card: 0x4A220C,
            hover: 0x6A3010,
            ink: 0xFFF1E6,
            dim: 0xE8B898,
            line: 0x7A3A14,
            accent: 0xFF8A3D,
            accent_ink: 0x1C0A04,
            tint: 0x6A2C0C,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0xC4A888,
        },
        "glacier" => Spec {
            bg: 0x14181C,
            wash: 0x1C2C38,
            surface: 0x1A2228,
            card: 0x222C34,
            hover: 0x2C3844,
            ink: 0xE8F2F6,
            dim: 0xA8BCC8,
            line: 0x344450,
            accent: 0x7EC8E0,
            accent_ink: 0x0C1820,
            tint: 0x1C3440,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x9EC4D4,
        },
        "plum" => Spec {
            bg: 0x18141A,
            wash: 0x321C30,
            surface: 0x221824,
            card: 0x2C2030,
            hover: 0x3A2840,
            ink: 0xF4E8F2,
            dim: 0xC0A8BC,
            line: 0x4A3448,
            accent: 0xD4A0C8,
            accent_ink: 0x1C1018,
            tint: 0x3A2438,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0xC4A8C8,
        },
        "copper" => Spec {
            bg: 0x161412,
            wash: 0x243028,
            surface: 0x1E1C18,
            card: 0x282420,
            hover: 0x36302A,
            ink: 0xF0EBE4,
            dim: 0xB8B0A4,
            line: 0x3E3A34,
            accent: 0x6FBFB0,
            accent_ink: 0x0E1816,
            tint: 0x243430,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0xA8C4BC,
        },
        "aurora" => Spec {
            bg: 0x041816,
            wash: 0x0E6A62,
            surface: 0x082420,
            card: 0x0E3834,
            hover: 0x14564E,
            ink: 0xE6FFFB,
            dim: 0x8EDDD4,
            line: 0x146E64,
            accent: 0x2EE6C7,
            accent_ink: 0x021412,
            tint: 0x0E5850,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8EC8B4,
        },
        _ => Spec {
            bg: 0x12151C,
            wash: 0x24304A,
            surface: 0x1A1E28,
            card: 0x242A38,
            hover: 0x323848,
            ink: 0xEEF1F8,
            dim: 0xA8B0C4,
            line: 0x3A445C,
            accent: 0x8AABFF,
            accent_ink: 0x10141C,
            tint: 0x2A3558,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
    }
}

/// Applies the default slate palette. Used before settings have loaded.
pub fn apply(theme: &mut Theme) {
    apply_palette(theme, "slate");
}

/// Applies one named palette. The window is always dark.
pub fn apply_palette(theme: &mut Theme, palette: &str) {
    let spec = spec_for(normalize_palette(palette));
    paint(theme, &spec);
    sync_controls(theme);
    // Palette paints replace the stock colors. The code highlighter is a
    // separate theme; leaving the light default on a dark page makes every
    // token the same color as the background, so fenced code shows no
    // highlight. Always install the dark syntax theme after a palette, then
    // lift comments toward the page ink. The stock gray sits too close to
    // these dark code cards.
    install_highlight(theme, &spec);
}

fn install_highlight(theme: &mut Theme, spec: &Spec) {
    let mut highlight = (*gpui_kit::component::highlighter::HighlightTheme::default_dark()).clone();
    let comment = syntax_color(comment_ink(spec));
    highlight.style.syntax.comment = Some(comment);
    highlight.style.syntax.comment_doc = Some(comment);
    theme.highlight_theme = std::sync::Arc::new(highlight);
}

/// Comments stay a step quieter than the page ink and still clear the card.
fn comment_ink(spec: &Spec) -> Hsla {
    soften(hex(spec.ink), hex(spec.dim), 0.18)
}

fn syntax_color(color: Hsla) -> gpui_kit::component::highlighter::ThemeStyle {
    let rgb = color.to_rgb();
    let channel = |value: f32| (value * 255.).round().clamp(0., 255.) as u8;
    let hex = format!(
        "#{:02X}{:02X}{:02X}",
        channel(rgb.r),
        channel(rgb.g),
        channel(rgb.b)
    );
    serde_json::from_str(&format!(r#"{{"color":"{hex}"}}"#)).expect("syntax color")
}

fn paint(theme: &mut Theme, spec: &Spec) {
    let bg = hex(spec.bg);
    // Keep enough of the palette wash that the six themes do not collapse
    // into the same gray. Chrome then paints this through a translucent fill.
    let wash = soften(hex(spec.wash), bg, 0.28);
    let surface = hex(spec.surface);
    let card = hex(spec.card);
    let hover = hex(spec.hover);
    let ink = hex(spec.ink);
    let dim = hex(spec.dim);
    let line = soften(hex(spec.line), bg, 0.32);
    let accent = hex(spec.accent);
    let accent_ink = hex(spec.accent_ink);
    let tint = hex(spec.tint);
    let green = hex(spec.green);
    let red = hex(spec.red);
    let info = hex(spec.info);

    theme.radius = gpui_kit::px(10.);
    theme.radius_lg = gpui_kit::px(12.);
    theme.shadow = false;

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
    // and bright enough to see on every palette.
    theme.selection = hex_a(spec.accent, 0.55);
    theme.primary = accent;
    theme.primary_foreground = accent_ink;
    theme.primary_hover = accent;
    theme.primary_active = accent;
    theme.link = accent;
    theme.link_hover = accent;
    theme.link_active = accent;
    theme.info = info;
    theme.info_foreground = accent_ink;
    theme.success = green;
    theme.success_foreground = accent_ink;
    theme.warning = accent;
    theme.warning_foreground = accent_ink;
    theme.danger = red;
    theme.danger_foreground = accent_ink;
    theme.input = line;
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
    theme.table_even = surface;
    theme.table_head = surface;
    theme.table_head_foreground = dim;
    theme.scrollbar = bg;
    theme.scrollbar_thumb = line;
    theme.scrollbar_thumb_hover = dim;
    theme.window_border = line;
    theme.overlay = hex_a(0x000000, 0.45);

    theme.green = green;
    theme.green_light = green;
    theme.red = red;
    theme.red_light = red;
    theme.blue = info;
    theme.blue_light = info;
    theme.yellow = accent;
    theme.yellow_light = accent;
    theme.magenta = accent;
    theme.magenta_light = accent;
    theme.cyan = info;
    theme.cyan_light = info;
}

/// `Theme::change` resets button tokens to the stock theme. Copy the
/// palette into both the legacy fields and `tokens`.
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

/// Semantic signal colors. `amber` follows the active palette accent so a
/// selected row is not always honey.
pub struct Desk {
    pub amber: Hsla,
    pub green: Hsla,
    pub red: Hsla,
    pub cyan: Hsla,
    pub violet: Hsla,
    pub faint: Hsla,
    pub screen: Hsla,
    pub screen_dim: Hsla,
}

impl Desk {
    pub fn of(theme: &Theme) -> Self {
        Self {
            amber: theme.primary,
            green: theme.green,
            red: theme.red,
            cyan: theme.cyan,
            violet: theme.magenta,
            faint: theme.muted_foreground,
            screen: theme.background,
            screen_dim: theme.muted_foreground,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn palette_ids_match_settings() {
        assert_eq!(
            super::PALETTES.as_slice(),
            mycode_config::VALID_PALETTES.as_slice()
        );
    }

    #[test]
    fn font_sizes_match_settings_and_land_on_the_body_scale() {
        assert_eq!(
            super::FONT_SIZES.as_slice(),
            mycode_config::VALID_FONT_SIZES.as_slice()
        );
        assert_eq!(super::interface_font_px("s"), 12.);
        assert_eq!(super::interface_font_px("m"), 13.);
        assert_eq!(super::interface_font_px("l"), 14.);
        assert_eq!(super::interface_font_px("xl"), 16.);
        assert_eq!(super::interface_font_px("nope"), 13.);
        assert!((super::interface_rem_px("l") - 16.).abs() < f32::EPSILON);
        assert_eq!(super::normalize_font_size("xl"), "xl");
        assert_eq!(super::font_size_label("s"), "S");
    }

    #[test]
    fn palette_row_is_six_fixed_cells() {
        assert_eq!(super::PALETTE_COLUMNS, 6);
        assert_eq!(super::PALETTE_SWATCH_PX, 28.);
        assert_eq!(super::PALETTE_CELL_PX, 52.);
        assert_eq!(super::PALETTE_GAP_PX, 6.);
        const _: () = assert!(super::PALETTE_CELL_PX > super::PALETTE_SWATCH_PX);
        assert_eq!(super::PALETTES.len(), 6);
        let columns = usize::from(super::PALETTE_COLUMNS);
        assert_eq!(super::PALETTES.len() / columns, 1);
        assert_eq!(super::PALETTES.len() % columns, 0);
        let row = super::palette_row_max_px();
        let six = super::PALETTE_CELL_PX * 6. + super::PALETTE_GAP_PX * 5.;
        assert!((row - six).abs() < f32::EPSILON);
    }

    /// The swatch must preview the page, not a solid accent chip. The colors
    /// are the same spec `apply_palette` writes onto the theme.
    #[test]
    fn palette_swatch_uses_the_same_spec_as_apply_palette() {
        for id in super::PALETTES {
            let spec = super::spec_for(id);
            let preview = super::palette_swatch_colors(id);
            assert_eq!(preview.base, super::hex(spec.bg));
            assert_eq!(preview.accent, super::hex(spec.accent));
            assert_ne!(spec.bg, spec.accent, "{id} base and accent must differ");
        }
        let slate = super::palette_swatch_colors("nope");
        let spec = super::spec_for("slate");
        assert_eq!(slate.base, super::hex(spec.bg));
        assert_eq!(slate.accent, super::hex(spec.accent));
    }

    #[test]
    fn font_family_choices_skip_faces_that_are_not_installed() {
        let faces: Vec<&str> = super::FONT_FACES.iter().map(|face| face.id).collect();
        assert_eq!(faces, mycode_config::VALID_FONT_FAMILIES);
        let installed = [
            "DejaVu Sans".to_owned(),
            "Noto Sans".to_owned(),
            "PingFang SC".to_owned(),
            "Inter Variable".to_owned(),
        ];
        assert_eq!(
            super::available_font_family_ids(&installed),
            ["system", "Inter", "PingFang", "Noto Sans"]
        );
        assert_eq!(
            super::paint_font_family("system", &installed),
            ".SystemUIFont"
        );
        assert_eq!(super::paint_font_family("", &installed), ".SystemUIFont");
        assert_eq!(
            super::paint_font_family("Inter", &installed),
            "Inter Variable"
        );
        assert_eq!(
            super::paint_font_family("PingFang", &installed),
            "PingFang SC"
        );
        assert_eq!(
            super::paint_font_family("Noto Sans", &installed),
            "Noto Sans"
        );
        assert_eq!(
            super::paint_font_family("Segoe UI", &installed),
            ".SystemUIFont"
        );
        let both = ["Inter".to_owned(), "Inter Variable".to_owned()];
        assert_eq!(super::paint_font_family("Inter", &both), "Inter");
    }

    /// Primary controls stay a tint: readable ink, a body that lifts off the
    /// card, and an edge that still meets a 3:1 boundary.
    #[test]
    fn primary_fill_stays_soft_and_readable_on_every_dark_palette() {
        for id in super::PALETTES {
            let spec = super::spec_for(id);
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
                "{id}: ink on the primary fill is {text:.2}, want >= 4.5"
            );
            assert!(
                (1.25..=2.2).contains(&body),
                "{id}: fill against the card is {body:.2}, want a soft 1.25..=2.2"
            );
            assert!(
                boundary >= 3.0,
                "{id}: primary edge against the card is {boundary:.2}, want >= 3"
            );
            let comment = super::comment_ink(&spec);
            let comments = contrast(comment, card);
            assert!(
                comments >= 7.0,
                "{id}: comment ink on the card is {comments:.2}, want >= 7"
            );
            let mut theme = gpui_kit::component::theme::Theme::default();
            super::apply_palette(&mut theme, id);
            let painted = theme
                .highlight_theme
                .style("comment")
                .and_then(|style| style.color)
                .expect("comment color");
            let painted_ratio = contrast(painted, card);
            assert!(
                painted_ratio >= 7.0,
                "{id}: installed comment color contrast is {painted_ratio:.2}"
            );
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
