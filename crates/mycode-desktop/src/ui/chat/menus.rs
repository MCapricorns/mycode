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
use crate::view_model::{MentionGroup, MentionKind, selected_reasoning_level};
use crate::workspace::Workspace;

#[cfg(all(test, feature = "test-support"))]
mod layout_tests;

/// Fixed row height that keeps the menu rows visually uniform.
const MENU_ROW_HEIGHT: gpui_kit::Pixels = px(34.);

/// The model picker, anchored above the model chip. Same provider step
/// and search list the settings page uses.
pub(super) fn render_model_menu(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    composer_menu_layer(
        "model-menu-layer",
        model_menu_anchor(),
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

/// Gap between the jump-to-latest button and a menu above it, in pixels.
const JUMP_LATEST_GAP: f32 = 8.;

/// Clearance that already keeps a menu above the composer card.
const COMPOSER_CARD_CLEARANCE: f32 = 88.;

/// Pixels from the composer bottom edge to the bottom of a chip menu.
///
/// The chips sit inside the composer card (input row, gap, and padding above
/// them). The base clearance puts the menu fully above that card. The jump
/// button is the next row up, so the menu also clears that button. The menu
/// paints with the composer, after the jump row; the extra gap is what keeps
/// their pixels from stacking.
/// Must keep the card clearance plus the jump row so the menu clears both
/// instead of sitting on the card's top edge.
pub(super) const COMPOSER_MENU_CLEARANCE: f32 =
    COMPOSER_CARD_CLEARANCE + JUMP_LATEST_BUTTON_PX + JUMP_LATEST_ROW_PAD + JUMP_LATEST_GAP;

/// Height cap that keeps an upward-opening menu inside the window.
///
/// The layer's bottom edge sits [`COMPOSER_MENU_CLEARANCE`] above the chip,
/// and the chip itself rests near the composer's bottom padding. In a short
/// window that leaves less room than the panel cap, so the panel is cut to
/// what fits (at least three rows) and scrolls inside instead of losing its
/// top rows to the window edge.
pub(super) fn menu_height_cap(window: &Window, cap: f32) -> gpui_kit::Pixels {
    let below_menu = COMPOSER_MENU_CLEARANCE + 72.;
    let headroom = 32.;
    let available = f32::from(window.viewport_size().height) - below_menu - headroom;
    px(available.clamp(110., cap))
}

/// The thinking menu shares the chip's right edge so it stays inside the column.
#[must_use]
pub(super) fn thinking_menu_anchor() -> MenuAnchor {
    MenuAnchor::ChipEnd
}

/// The model menu shares the chip's left edge, above the model button.
#[must_use]
pub(super) fn model_menu_anchor() -> MenuAnchor {
    MenuAnchor::ChipStart
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
    let layer = div().id(id).absolute().bottom(px(COMPOSER_MENU_CLEARANCE));
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
        thinking_menu_anchor(),
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
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .rounded(skin::radius_control())
        .text_sm()
        .cursor_pointer()
        .when(highlighted, |this| this.bg(skin::frost(theme)))
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
/// the composer like the model menu. Slash rows stay one flat list: group
/// headings only added divisions to scroll past.
pub(super) fn render_mention_layer(
    workspace: &mut Workspace,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let mention = workspace
        .vm()
        .mention
        .as_ref()
        .expect("caller checks the menu is open");
    let heading = match mention.kind {
        MentionKind::File => Some(t("FILES", "文件")),
        MentionKind::Command => None,
    };
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
            div().w_full().child(
                menu_panel("mention-menu", theme)
                    .w_full()
                    .max_h(menu_height_cap(window, 400.))
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when_some(heading, |this, heading| {
                        this.child(
                            div()
                                .text_xs()
                                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                                .opacity(0.6)
                                .px_2()
                                .pt_1()
                                .child(heading),
                        )
                    })
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
    use super::{
        COMPOSER_MENU_CLEARANCE, JUMP_LATEST_BUTTON_PX, JUMP_LATEST_ROW_PAD, MenuAnchor,
        model_menu_anchor, thinking_menu_anchor,
    };

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

    #[test]
    fn thinking_menu_clears_the_jump_button() {
        const {
            assert!(
                COMPOSER_MENU_CLEARANCE >= 88. + JUMP_LATEST_BUTTON_PX + JUMP_LATEST_ROW_PAD + 8.
            );
        }
        assert_eq!(thinking_menu_anchor(), MenuAnchor::ChipEnd);
    }

    #[test]
    fn composer_menus_anchor_to_their_chips() {
        assert_eq!(thinking_menu_anchor(), MenuAnchor::ChipEnd);
        assert_eq!(model_menu_anchor(), MenuAnchor::ChipStart);
    }
}
