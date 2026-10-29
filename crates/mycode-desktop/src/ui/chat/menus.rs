//! The docked composer menus: the model picker, the thinking-effort submenu,
//! and the `@`/`/` mention autocomplete.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, anchored, deferred, div, px,
};

use crate::i18n::t;
use crate::ui::skin::{self, menu_panel};
use crate::view_model::{MentionGroup, selected_reasoning_level};
use crate::workspace::Workspace;

#[cfg(all(test, feature = "test-support"))]
mod layout_tests;

/// Fixed row height that keeps the menu rows visually uniform.
const MENU_ROW_HEIGHT: gpui_kit::Pixels = px(34.);
/// Maximum rows visible at once in the `@`/`/` autocomplete menu; longer lists
/// scroll by arrow keys or the mouse wheel.
const MENTION_VISIBLE_ROWS: usize = 3;
/// Height cap for [`MENTION_VISIBLE_ROWS`] rows (`34px` each), two `gap_1`
/// gaps (`4px`), `p_1` padding (`8px`), and the `1px` top/bottom border.
pub(super) const MENTION_MENU_MAX_H: f32 =
    MENTION_VISIBLE_ROWS as f32 * 34. + (MENTION_VISIBLE_ROWS - 1) as f32 * 4. + 8. + 2.;

/// The model picker, anchored above the model chip. Same provider step
/// and search list the settings page uses.
pub(super) fn render_model_menu(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    composer_menu_layer(
        "model-menu-layer",
        MenuAnchor::ChipStart,
        crate::ui::model_picker::render_model_picker(
            workspace,
            window,
            crate::ui::model_picker::ModelPickerTarget::Session,
            menu_height_cap(window, 420.),
            cx,
        ),
    )
    .into_any_element()
}

/// Which edge of the chip the menu shares, so a wide menu grows into the column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuAnchor {
    /// Left edge of the chip. The model menu grows right.
    ChipStart,
    /// Right edge of the chip. The thinking menu grows left.
    ChipEnd,
}

/// Diameter of the jump-to-latest button, in pixels.
pub(super) const JUMP_LATEST_BUTTON_PX: f32 = 30.;

/// Padding under the jump-to-latest row, in pixels.
pub(super) const JUMP_LATEST_ROW_PAD: f32 = 4.;

/// Gap between a composer menu and its trigger button.
const COMPOSER_MENU_GAP: f32 = 8.;

/// Height cap for an upward-opening menu. Reserve room for the footer
/// (including a wrapped row) and top margin; the anchor follows the button.
pub(super) fn menu_height_cap(window: &Window, cap: f32) -> gpui_kit::Pixels {
    let below_menu = COMPOSER_MENU_GAP + 72.;
    let headroom = 32.;
    let available = f32::from(window.viewport_size().height) - below_menu - headroom;
    px(available.clamp(110., cap))
}

/// Keep the preferred chip edge, but clamp the panel when a wrapped footer
/// puts that chip near a window edge. Deferred paint escapes the chat's clip
/// and keeps menus above the surrounding chrome; the panel still occludes
/// controls underneath it.
fn composer_menu_layer(
    id: &'static str,
    anchor: MenuAnchor,
    panel: impl IntoElement,
) -> impl IntoElement {
    let layer = div()
        .id(id)
        .absolute()
        .bottom_full()
        .mb(px(COMPOSER_MENU_GAP));
    let (layer, corner) = match anchor {
        MenuAnchor::ChipStart => (layer.left_0(), gpui_kit::Anchor::BottomLeft),
        MenuAnchor::ChipEnd => (layer.right_0(), gpui_kit::Anchor::BottomRight),
    };
    layer.child(
        deferred(
            anchored()
                .anchor(corner)
                .snap_to_window_with_margin(px(8.))
                .child(div().occlude().child(panel)),
        )
        .with_priority(1),
    )
}

/// Thinking effort as its own short list, opened from the composer button.
pub(super) fn render_thinking_menu(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let levels = crate::view_model::selected_reasoning_levels(workspace.vm());
    let selected = selected_reasoning_level(workspace.vm());
    let preferred = workspace.reasoning_highlight(&levels);
    let scroll = workspace.reasoning_scroll().clone();
    let weak = cx.weak_entity();
    composer_menu_layer(
        "thinking-menu-layer",
        MenuAnchor::ChipEnd,
        menu_panel("thinking-menu", theme)
            .w(px(220.))
            .max_h(menu_height_cap(window, 320.))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex_none()
            .p_1()
            .flex()
            .flex_col()
            .gap_1()
            .children(levels.iter().enumerate().map(|(index, level)| {
                let picked = level.clone();
                let weak = weak.clone();
                let on = level == &selected;
                menu_row(
                    format!("thinking-row-{level}"),
                    reasoning_row_label(level),
                    index == preferred,
                    on,
                    move |_, _, cx| {
                        let picked = picked.clone();
                        let _ = weak.update(cx, |workspace, cx| {
                            workspace.on_select_reasoning(&picked, cx);
                        });
                    },
                    theme,
                )
            })),
    )
    .into_any_element()
}

fn menu_row(
    id: String,
    label: String,
    highlighted: bool,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .h(MENU_ROW_HEIGHT)
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .rounded(skin::radius_control())
        .text_sm()
        .cursor_pointer()
        .when(highlighted, |this| this.bg(skin::frost_hover(theme)))
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click(on_click)
        .child(div().min_w_0().truncate().child(label))
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .flex_shrink_0()
                    .text_color(theme.foreground),
            )
        })
}

/// Thinking-effort labels stay English in both UI languages: the rung names
/// are short technical tokens and the localized variants were clipped by the
/// chip's narrow width.
pub(crate) fn reasoning_row_label(level: &str) -> String {
    match level {
        "default" => "Default · provider".to_owned(),
        "off" => "Off".to_owned(),
        "on" => "On".to_owned(),
        "minimal" => "Minimal".to_owned(),
        "low" => "Low · brief".to_owned(),
        "medium" => "Medium · balanced".to_owned(),
        "high" => "High · deep".to_owned(),
        "xhigh" => "Extra high".to_owned(),
        "max" => "Max".to_owned(),
        other => other.to_owned(),
    }
}

/// The `@` file and `/` command autocomplete panel, docked in-flow above
/// the composer and sharing its horizontal measure. Capped at three rows so
/// longer lists scroll via arrow keys or the mouse wheel.
pub(super) fn render_mention_layer(
    workspace: &mut Workspace,
    _window: &Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let mention = workspace
        .vm()
        .mention
        .as_ref()
        .expect("caller checks the menu is open");
    let preferred = workspace.mention_highlight();
    let scroll = workspace.mention_scroll().clone();
    let mut rows: Vec<gpui_kit::AnyElement> = Vec::new();
    for (index, item) in mention.items.iter().enumerate() {
        let label = mention_label(item);
        let row_id = mention_row_id(index, item);
        rows.push(
            menu_row(
                row_id,
                label,
                index == preferred,
                false,
                cx.listener(move |workspace, _, window, cx| {
                    workspace.on_accept_mention(index, window, cx);
                }),
                theme,
            )
            .into_any_element(),
        );
    }
    div()
        .id("mention-layer")
        .w_full()
        .px_4()
        .pb_1()
        .flex()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(px(super::composer::COMPOSER_MAX_PX))
                .min_w_0()
                .child(
                    menu_panel("mention-menu", theme)
                        .occlude()
                        .w_full()
                        .max_h(px(MENTION_MENU_MAX_H))
                        .overflow_y_scroll()
                        .track_scroll(&scroll)
                        .p_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(rows),
                ),
        )
        .into_any_element()
}

fn mention_row_id(index: usize, item: &crate::view_model::MentionItem) -> String {
    format!("mention-{index}-{:?}-{}", item.group, item.insert)
}

fn mention_label(item: &crate::view_model::MentionItem) -> String {
    match item.group {
        MentionGroup::Command | MentionGroup::Skill => {
            format!("{} · {}", item.insert, item.label)
        }
        MentionGroup::Mcp => {
            if let Some((server, tool)) = item
                .insert
                .strip_prefix("mcp:")
                .and_then(|rest| rest.split_once('/'))
            {
                format!("/{server}/{tool} · {}", t("MCP tool", "MCP 工具"))
            } else {
                let server = item.insert.strip_prefix("mcp:").unwrap_or(&item.label);
                format!("/{server} · {}", t("MCP server", "MCP 服务"))
            }
        }
        MentionGroup::File => item.label.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{MENTION_MENU_MAX_H, MENTION_VISIBLE_ROWS};

    #[test]
    fn mention_menu_caps_at_three_rows_and_composer_width() {
        const {
            assert!(MENTION_VISIBLE_ROWS == 3);
            assert!(MENTION_MENU_MAX_H == 120.);
            assert!(super::super::composer::COMPOSER_MAX_PX == 760.);
        }
    }

    #[test]
    fn builtin_and_skill_rows_with_the_same_slug_have_distinct_ids() {
        let command = crate::view_model::MentionItem {
            insert: "/new".to_owned(),
            label: "new".to_owned(),
            group: crate::view_model::MentionGroup::Command,
        };
        let skill = crate::view_model::MentionItem {
            group: crate::view_model::MentionGroup::Skill,
            ..command.clone()
        };
        assert_ne!(
            super::mention_row_id(0, &command),
            super::mention_row_id(1, &skill)
        );
    }
}
