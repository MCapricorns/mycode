//! Dark palettes. Surfaces are solid. The page background is a
//! two-stop gradient so the chat column shows the falloff; rails and dialogs
//! stay opaque so text never sits on a washed-out fill.
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
pub const PALETTES: [&str; 13] = [
    "slate", "ocean", "forest", "dusk", "sand", "rose", "ink", "moss", "ember", "glacier", "plum",
    "copper", "aurora",
];

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
        "sand" => t("Sand", "沙丘"),
        "rose" => t("Rose", "玫瑰"),
        "ink" => t("Ink", "墨色"),
        "moss" => t("Moss", "苔原"),
        "ember" => t("Ember", "余烬"),
        "glacier" => t("Glacier", "冰川"),
        "plum" => t("Plum", "梅紫"),
        "copper" => t("Copper", "铜绿"),
        "aurora" => t("Aurora", "极光"),
        _ => t("Slate", "石板灰"),
    }
}

/// Accent swatch for the settings picker.
#[must_use]
pub fn palette_swatch(palette: &str) -> Hsla {
    hex(spec_for(normalize_palette(palette)).accent)
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
            bg: 0x0E1A20,
            wash: 0x12343C,
            surface: 0x15242C,
            card: 0x1C3038,
            hover: 0x254048,
            ink: 0xE6F3F4,
            dim: 0x9BB8BE,
            line: 0x2C4A52,
            accent: 0x3EC6C0,
            accent_ink: 0x06201E,
            tint: 0x1A3C40,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "forest" => Spec {
            bg: 0x121814,
            wash: 0x1A2A1E,
            surface: 0x1A221C,
            card: 0x222C24,
            hover: 0x2C3A30,
            ink: 0xE7F0E8,
            dim: 0xA3B8A8,
            line: 0x334238,
            accent: 0x6FBF8A,
            accent_ink: 0x0E1A12,
            tint: 0x24382A,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8FB4C4,
        },
        "dusk" => Spec {
            bg: 0x16141C,
            wash: 0x261C34,
            surface: 0x1E1A26,
            card: 0x282232,
            hover: 0x342C42,
            ink: 0xEDE8F4,
            dim: 0xB4A8C4,
            line: 0x3C344C,
            accent: 0xC4B5FD,
            accent_ink: 0x1A1424,
            tint: 0x322848,
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
            bg: 0x1A1410,
            wash: 0x3A2418,
            surface: 0x241C16,
            card: 0x2E241C,
            hover: 0x3C3024,
            ink: 0xF6EDE4,
            dim: 0xC4B0A0,
            line: 0x4A382C,
            accent: 0xE08A4A,
            accent_ink: 0x1C1008,
            tint: 0x3A2818,
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
            bg: 0x101418,
            wash: 0x142830,
            surface: 0x161C22,
            card: 0x1E262C,
            hover: 0x28343A,
            ink: 0xE6F4F0,
            dim: 0x9CB4B0,
            line: 0x2C4044,
            accent: 0x5ED4A0,
            accent_ink: 0x081410,
            tint: 0x143028,
            green: 0x8FBF9F,
            red: 0xE08B7A,
            info: 0x8EC8B4,
        },
        _ => Spec {
            bg: 0x171A20,
            wash: 0x1E2A3A,
            surface: 0x22262E,
            card: 0x2A303A,
            hover: 0x343B48,
            ink: 0xE8EAF0,
            dim: 0xA7B0BE,
            line: 0x3A4250,
            accent: 0x7AA2F7,
            accent_ink: 0x10141C,
            tint: 0x2A3550,
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
}

fn paint(theme: &mut Theme, spec: &Spec) {
    let bg = hex(spec.bg);
    let wash = hex(spec.wash);
    let surface = hex(spec.surface);
    let card = hex(spec.card);
    let hover = hex(spec.hover);
    let ink = hex(spec.ink);
    let dim = hex(spec.dim);
    let line = hex(spec.line);
    let accent = hex(spec.accent);
    let accent_ink = hex(spec.accent_ink);
    let tint = hex(spec.tint);
    let green = hex(spec.green);
    let red = hex(spec.red);
    let info = hex(spec.info);

    theme.radius = gpui_kit::px(8.);
    theme.radius_lg = gpui_kit::px(12.);
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
    // Painted over the glyphs, so an opaque fill hides the selected text.
    theme.selection = hex_a(spec.accent, 0.38);
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
    theme.button_primary = theme.primary;
    theme.button_primary_hover = theme.primary_hover;
    theme.button_primary_active = theme.primary_active;
    theme.button_primary_foreground = theme.primary_foreground;
    theme.tokens.button_primary = theme.primary.into();
    theme.tokens.button_primary_hover = theme.primary_hover.into();
    theme.tokens.button_primary_active = theme.primary_active.into();
    theme.tokens.button_primary_foreground = theme.primary_foreground.into();

    theme.button = theme.secondary;
    theme.button_hover = theme.secondary_hover;
    theme.button_active = theme.secondary_active;
    theme.button_foreground = theme.foreground;
    theme.tokens.button = theme.secondary.into();
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
}
