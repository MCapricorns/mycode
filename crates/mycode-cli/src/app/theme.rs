//! Terminal ink for the chosen appearance.
//!
//! A terminal owns its own background, so `dark` and `light` do not repaint
//! the window: they choose ink that stays readable on that background. Prose
//! always keeps the terminal's own foreground; only dim text, accents, and
//! status colors are picked here. The ANSI named colors follow the terminal's
//! palette, which is why code blocks stay out of this.

use mycode_frontend::view_model::WorkspaceState;
use ratatui::style::Color;

use super::CRAB;

/// One appearance's ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    /// Secondary text: hints, tool results, borders, the status bar.
    pub(crate) dim: Color,
    /// The assistant marker and the crab greeting.
    pub(crate) accent: Color,
    /// The user's own message marker.
    pub(crate) user: Color,
    /// A failed turn or a refused action.
    pub(crate) error: Color,
    /// Queued follow-ups.
    pub(crate) warning: Color,
    /// Tool calls and informational notices.
    pub(crate) info: Color,
    /// Thinking traces and steering text.
    pub(crate) thinking: Color,
}

impl Palette {
    /// Ink for a dark terminal.
    pub(crate) const DARK: Self = Self {
        dim: Color::DarkGray,
        accent: CRAB,
        user: Color::Green,
        error: Color::Red,
        warning: Color::Yellow,
        info: Color::Cyan,
        thinking: Color::Magenta,
    };

    /// Ink for a light terminal. `DarkGray` is nearly invisible on white, so
    /// the dim text becomes a readable gray and the warm accents darken
    /// enough to keep their contrast.
    pub(super) const LIGHT: Self = Self {
        dim: Color::Rgb(0x5f, 0x5f, 0x5f),
        accent: Color::Rgb(0xb5, 0x48, 0x00),
        user: Color::Rgb(0x1a, 0x7f, 0x37),
        error: Color::Rgb(0xb3, 0x1d, 0x1d),
        warning: Color::Rgb(0x7a, 0x52, 0x00),
        info: Color::Rgb(0x0b, 0x5c, 0x9e),
        thinking: Color::Rgb(0x7b, 0x2d, 0x8f),
    };

    /// The palette the settings ask for; a missing or unreadable document is
    /// dark, the default appearance.
    pub(super) fn of(vm: &WorkspaceState) -> Self {
        let theme = vm
            .settings
            .as_ref()
            .map(|settings| settings.theme.as_str())
            .unwrap_or("dark");
        if theme == "light" {
            Self::LIGHT
        } else {
            Self::DARK
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CRAB, Palette};
    use mycode_frontend::view_model::WorkspaceState;

    #[test]
    fn the_light_appearance_uses_ink_that_reads_on_white() {
        use ratatui::style::Color;
        assert_ne!(Palette::LIGHT, Palette::DARK);
        // Every slot changes, not only the dim text.
        assert_eq!(
            Palette::LIGHT,
            Palette {
                dim: Color::Rgb(0x5f, 0x5f, 0x5f),
                accent: Color::Rgb(0xb5, 0x48, 0x00),
                user: Color::Rgb(0x1a, 0x7f, 0x37),
                error: Color::Rgb(0xb3, 0x1d, 0x1d),
                warning: Color::Rgb(0x7a, 0x52, 0x00),
                info: Color::Rgb(0x0b, 0x5c, 0x9e),
                thinking: Color::Rgb(0x7b, 0x2d, 0x8f),
            }
        );
        // The default stays byte-for-byte the ANSI names the terminal had
        // before the palette existed.
        assert_eq!(
            Palette::DARK,
            Palette {
                dim: Color::DarkGray,
                accent: CRAB,
                user: Color::Green,
                error: Color::Red,
                warning: Color::Yellow,
                info: Color::Cyan,
                thinking: Color::Magenta,
            }
        );
        let dark = Palette::of(&WorkspaceState::default());
        assert_eq!(dark, Palette::DARK, "no settings yet means dark");
        let mut light = WorkspaceState {
            settings: Some(mycode_frontend::view_model::SettingsState::from_settings(
                &mycode_config::AppSettings::default(),
                1,
            )),
            ..WorkspaceState::default()
        };
        if let Some(settings) = light.settings.as_mut() {
            settings.theme = "light".to_owned();
        }
        assert_eq!(Palette::of(&light), Palette::LIGHT);
    }
}
