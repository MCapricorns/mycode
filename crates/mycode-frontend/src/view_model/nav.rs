//! Layered-dismissal and menu-navigation decisions every frontend shares.
//!
//! Escape closes exactly one floating layer per press, and pickers wrap or
//! clamp their highlight the same way on every frontend, so the ordering
//! lives here instead of beside one renderer.

use super::settings::{McpSubview, ModelsSubview, SettingsSection};

/// One floating layer Escape can dismiss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DismissLayer {
    /// The in-app folder picker.
    Picker,
    /// The dedicated git-diff panel.
    Diff,
    /// The working-tree changes drawer.
    Changes,
    /// The unpinned Details overlay.
    Inspector,
}

/// The single layer one Escape press closes. Later flags stay open.
pub fn top_dismiss_layer(
    picker: bool,
    diff: bool,
    changes: bool,
    inspector_overlay: bool,
) -> Option<DismissLayer> {
    if picker {
        Some(DismissLayer::Picker)
    } else if diff {
        Some(DismissLayer::Diff)
    } else if changes {
        Some(DismissLayer::Changes)
    } else if inspector_overlay {
        Some(DismissLayer::Inspector)
    } else {
        None
    }
}

/// Where one Escape press goes inside Settings. Each arm matches the back
/// control that page actually draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsBack {
    /// Provider detail's back returns to the provider list.
    CloseProviderDetail,
    /// A catalog preset's back returns to the catalog, not the provider list.
    ClosePreset,
    /// Catalog or custom-endpoint back returns to the provider list.
    ModelsList,
    /// A web-vendor detail's back returns to the backend list.
    WebList,
    /// Catalog, JSON, or custom MCP back returns to the server list.
    McpList,
    /// Back to desk.
    Desk,
}

/// The settings page Escape should open. Sub-pages step back once.
pub fn settings_back_target(
    section: SettingsSection,
    models: ModelsSubview,
    provider_open: bool,
    preset_open: bool,
    web_detail_open: bool,
    mcp: McpSubview,
) -> SettingsBack {
    match section {
        SettingsSection::Models => match models {
            ModelsSubview::List if provider_open => SettingsBack::CloseProviderDetail,
            ModelsSubview::Catalog if preset_open => SettingsBack::ClosePreset,
            ModelsSubview::Catalog | ModelsSubview::OAuth | ModelsSubview::Custom => {
                SettingsBack::ModelsList
            }
            ModelsSubview::List => SettingsBack::Desk,
        },
        SettingsSection::Web if web_detail_open => SettingsBack::WebList,
        SettingsSection::Mcp if mcp != McpSubview::List => SettingsBack::McpList,
        _ => SettingsBack::Desk,
    }
}

/// The picker row a highlight lands on: an explicit selection wins, else the
/// preferred row, clamped into range.
pub fn highlight(selected: Option<usize>, len: usize, preferred: usize) -> usize {
    selected.unwrap_or(preferred).min(len.saturating_sub(1))
}

/// One arrow step through a picker list, wrapping at both ends.
pub fn moved(current: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        0
    } else if down {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DismissLayer, SettingsBack, highlight, moved, settings_back_target, top_dismiss_layer,
    };
    use crate::view_model::{McpSubview, ModelsSubview, SettingsSection};

    #[test]
    fn escape_closes_one_layer_topmost_first() {
        assert_eq!(
            top_dismiss_layer(false, true, true, true),
            Some(DismissLayer::Diff)
        );
        assert_eq!(
            top_dismiss_layer(false, false, true, true),
            Some(DismissLayer::Changes)
        );
        assert_eq!(
            top_dismiss_layer(false, false, false, true),
            Some(DismissLayer::Inspector)
        );
        assert_eq!(
            top_dismiss_layer(true, true, true, true),
            Some(DismissLayer::Picker)
        );
        assert_eq!(top_dismiss_layer(false, false, false, false), None);
    }

    #[test]
    fn settings_escape_matches_the_visible_back_button() {
        let desk = |section| {
            settings_back_target(
                section,
                ModelsSubview::List,
                false,
                false,
                false,
                McpSubview::List,
            )
        };
        assert_eq!(desk(SettingsSection::General), SettingsBack::Desk);
        assert_eq!(desk(SettingsSection::About), SettingsBack::Desk);
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::List,
                true,
                false,
                false,
                McpSubview::List,
            ),
            SettingsBack::CloseProviderDetail
        );
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::Catalog,
                false,
                true,
                false,
                McpSubview::List,
            ),
            SettingsBack::ClosePreset
        );
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::Catalog,
                false,
                false,
                false,
                McpSubview::List,
            ),
            SettingsBack::ModelsList
        );
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::Custom,
                false,
                false,
                false,
                McpSubview::List,
            ),
            SettingsBack::ModelsList
        );
        // The shared OAuth sign-in page has no form: Escape leaves the page.
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::OAuth,
                false,
                false,
                false,
                McpSubview::List,
            ),
            SettingsBack::ModelsList
        );
        assert_eq!(
            settings_back_target(
                SettingsSection::Web,
                ModelsSubview::List,
                false,
                false,
                true,
                McpSubview::List,
            ),
            SettingsBack::WebList
        );
        for subview in [McpSubview::Catalog, McpSubview::Json, McpSubview::Custom] {
            assert_eq!(
                settings_back_target(
                    SettingsSection::Mcp,
                    ModelsSubview::List,
                    false,
                    false,
                    false,
                    subview,
                ),
                SettingsBack::McpList,
                "{subview:?}"
            );
        }
    }

    #[test]
    fn navigation_wraps_and_clamps_after_filtering() {
        assert_eq!(moved(0, 3, false), 2);
        assert_eq!(moved(2, 3, true), 0);
        assert_eq!(moved(0, 0, true), 0);
        assert_eq!(highlight(None, 5, 3), 3);
        assert_eq!(highlight(Some(4), 2, 0), 1);
        assert_eq!(highlight(None, 0, 7), 0);
    }
}
