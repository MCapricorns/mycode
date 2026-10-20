//! The Keyboard shortcuts settings page: one card per group of keys, each
//! row naming what the keys do beside the keys drawn as keycaps.
//!
//! The list is `mycode_frontend::shortcuts`, shared with the CLI settings.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div, px,
};
use mycode_frontend::shortcuts::{Group, Shortcut, Surface, shortcuts};

use super::widgets::{settings_card, settings_row};
use crate::workspace::Workspace;

/// Drawn between the keycaps of one chord. macOS writes chords without
/// one (`⌘G`).
const CHORD_JOINER: Option<&str> = if cfg!(target_os = "macos") {
    None
} else {
    Some("+")
};

pub(super) fn render_shortcuts_section(cx: &mut Context<Workspace>) -> AnyElement {
    let theme = cx.theme();
    let list = shortcuts(Surface::Desktop);
    let cards = Group::ALL.map(|group| {
        let rows = list
            .iter()
            .filter(|shortcut| shortcut.group == group)
            .enumerate()
            .map(|(index, shortcut)| shortcut_row(group, index, shortcut, theme))
            .collect();
        settings_card(group.id(), group.label(), Some(group.hint()), theme, rows).into_any_element()
    });
    div()
        .id("shortcuts-section")
        .w_full()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap(crate::ui::skin::space(4))
        .children(cards)
        .into_any_element()
}

/// What the keys do on the left, the keycaps on the right.
fn shortcut_row(group: Group, index: usize, shortcut: &Shortcut, theme: &Theme) -> AnyElement {
    let mut keys = div().flex().flex_row().items_center().gap(px(4.));
    for (position, label) in keycap_labels(&shortcut.keys).into_iter().enumerate() {
        if position > 0
            && let Some(joiner) = CHORD_JOINER
        {
            keys = keys.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(joiner),
            );
        }
        keys = keys.child(keycap(label, theme));
    }
    settings_row(
        &format!("shortcut-{}-{index}", group.id()),
        shortcut.action,
        shortcut.detail,
        keys.into_any_element(),
        theme,
    )
}

/// One key as a raised cap: a hairline rim with a heavier lower edge.
fn keycap(label: &str, theme: &Theme) -> impl IntoElement {
    div()
        .h(px(24.))
        .min_w(px(24.))
        .px(px(7.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(crate::ui::skin::radius_xs())
        .border_1()
        .border_b_2()
        .border_color(crate::ui::skin::border_strong(theme))
        .bg(theme.secondary)
        .text_xs()
        .text_color(theme.foreground)
        .child(label.to_owned())
}

/// Splits a spelled chord into its keys: `Ctrl+Enter` into `Ctrl` and
/// `Enter`, macOS `⌘G` into `⌘` and `G`.
fn keycap_labels(keys: &str) -> Vec<&str> {
    let mut labels = Vec::new();
    let rest = match keys.strip_prefix('⌘') {
        Some(rest) => {
            labels.push("⌘");
            rest
        }
        None => keys,
    };
    labels.extend(rest.split('+').filter(|label| !label.is_empty()));
    labels
}

#[cfg(test)]
mod tests {
    use super::keycap_labels;

    #[test]
    fn a_chord_splits_into_one_cap_per_key() {
        assert_eq!(keycap_labels("Ctrl+Enter"), ["Ctrl", "Enter"]);
        assert_eq!(keycap_labels("Shift+Enter"), ["Shift", "Enter"]);
        assert_eq!(keycap_labels("⌘G"), ["⌘", "G"]);
        assert_eq!(keycap_labels("⌘Enter"), ["⌘", "Enter"]);
        assert_eq!(keycap_labels("Esc"), ["Esc"]);
    }

    #[test]
    fn every_desktop_shortcut_draws_clean_caps() {
        use mycode_frontend::shortcuts::{Surface, shortcuts};
        for shortcut in shortcuts(Surface::Desktop) {
            let labels = keycap_labels(&shortcut.keys);
            assert!(!labels.is_empty(), "{}", shortcut.keys);
            for label in labels {
                assert!(
                    !label.contains('+') && !label.contains(char::is_whitespace),
                    "{}",
                    shortcut.keys
                );
            }
        }
    }
}
