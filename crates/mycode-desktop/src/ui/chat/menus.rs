//! The docked composer menus: the model picker, the thinking-effort submenu,
//! and the `@`/`/` mention autocomplete.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::i18n::t;
use crate::ui::skin::{self, menu_panel};
use crate::view_model::{
    MentionGroup, MentionKind, preferred_slash_index, selected_reasoning_level,
};
use crate::workspace::Workspace;

/// Fixed row height that keeps the menu rows visually uniform.
const MENU_ROW_HEIGHT: gpui_kit::Pixels = px(30.);

/// The model picker, anchored above the model chip. Same provider step
/// and search list the settings page uses.
pub(super) fn render_model_menu(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    debug_assert_eq!(model_menu_anchor(), MenuAnchor::ChipStart);
    anchor_edge(
        div()
            .id("model-menu-layer")
            .absolute()
            .bottom(px(THINKING_MENU_CLEARANCE))
            .occlude(),
        model_menu_anchor(),
    )
    .child(crate::ui::model_picker::render_model_picker(
        workspace,
        window,
        crate::ui::model_picker::ModelPickerTarget::Session,
        menu_height_cap(window, 420.),
        cx,
    ))
    .into_any_element()
}

/// Where a composer menu sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuPlacement {
    /// Directly above its chip. The transcript is not clipped.
    AboveChip,
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

/// Pixels from the bottom of the Thinking chip to the bottom of the menu.
///
/// The chip sits inside the composer card (input row, gap, and padding above
/// it). The base clearance puts the menu fully above that card. The jump
/// button is the next row up, so the menu also clears that button. The menu
/// paints with the composer, after the jump row; the extra gap is what keeps
/// their pixels from stacking.
/// Must stay at or above 84 so the docked menu clears the composer card
/// instead of sitting on its top edge.
pub(super) const THINKING_MENU_CLEARANCE: f32 =
    COMPOSER_CARD_CLEARANCE + JUMP_LATEST_BUTTON_PX + JUMP_LATEST_ROW_PAD + JUMP_LATEST_GAP;

/// Height cap that keeps an upward-opening menu inside the window.
///
/// The layer's bottom edge sits [`THINKING_MENU_CLEARANCE`] above the chip,
/// and the chip itself rests near the composer's bottom padding. In a short
/// window that leaves less room than the panel cap, so the panel is cut to
/// what fits (at least three rows) and scrolls inside instead of losing its
/// top rows to the window edge.
pub(super) fn menu_height_cap(window: &Window, cap: f32) -> gpui_kit::Pixels {
    let below_menu = THINKING_MENU_CLEARANCE + 72.;
    let headroom = 32.;
    let available = f32::from(window.viewport_size().height) - below_menu - headroom;
    px(available.clamp(110., cap))
}

/// The thinking menu opens above its chip.
#[must_use]
pub(super) fn thinking_menu_placement() -> MenuPlacement {
    MenuPlacement::AboveChip
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

fn anchor_edge(
    layer: gpui_kit::Stateful<gpui_kit::Div>,
    anchor: MenuAnchor,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    match anchor {
        MenuAnchor::ChipStart => layer.left_0(),
        MenuAnchor::ChipEnd => layer.right_0(),
    }
}

/// Thinking effort as its own short list, opened from the composer button.
pub(super) fn render_thinking_menu(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let levels = crate::view_model::selected_reasoning_levels(workspace.vm());
    let selected = selected_reasoning_level(workspace.vm()).to_owned();
    let weak = cx.weak_entity();
    debug_assert_eq!(thinking_menu_placement(), MenuPlacement::AboveChip);
    debug_assert_eq!(thinking_menu_anchor(), MenuAnchor::ChipEnd);
    anchor_edge(
        div()
            .id("thinking-menu-layer")
            .absolute()
            .bottom(px(THINKING_MENU_CLEARANCE))
            .occlude(),
        thinking_menu_anchor(),
    )
    .child(
        menu_panel("thinking-menu", theme)
            .w(px(220.))
            .max_h(menu_height_cap(window, 320.))
            .overflow_y_scroll()
            .flex_none()
            .p_1()
            .flex()
            .flex_col()
            .gap_1()
            .children(levels.iter().map(|level| {
                let picked = level.clone();
                let weak = weak.clone();
                let on = level == &selected;
                menu_row(
                    format!("thinking-row-{level}"),
                    reasoning_row_label(level),
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
        .when(selected, |this| this.bg(skin::frost_accent(theme)))
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click(on_click)
        .child(div().min_w_0().truncate().child(label))
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .flex_shrink_0()
                    .text_color(theme.primary),
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
/// the composer like the model menu.
pub(super) fn render_mention_layer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let mention = workspace
        .vm()
        .mention
        .clone()
        .expect("caller checks the menu is open");
    let heading = match mention.kind {
        MentionKind::File => Some(t("FILES", "文件")),
        MentionKind::Command => None,
    };
    let preferred = if mention.kind == MentionKind::Command {
        preferred_slash_index(&mention.fragment, &mention.items)
    } else {
        0
    };
    let mut rows: Vec<gpui_kit::AnyElement> = Vec::new();
    let mut previous: Option<MentionGroup> = None;
    for (index, item) in mention.items.iter().enumerate() {
        if mention.kind == MentionKind::Command && previous != Some(item.group) {
            previous = Some(item.group);
            rows.push(slash_heading(item.group, theme));
        }
        let insert = item.insert.clone();
        let label = mention_label(item);
        let row_id = format!("mention-{insert}");
        rows.push(
            menu_row(
                row_id,
                label,
                index == preferred,
                cx.listener(move |workspace, _, window, cx| {
                    workspace.on_accept_mention(insert.clone(), window, cx);
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
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when_some(heading, |this, heading| {
                        this.child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
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

fn slash_heading(group: MentionGroup, theme: &Theme) -> gpui_kit::AnyElement {
    let label = match group {
        MentionGroup::Command => t("Commands", "命令"),
        MentionGroup::Skill => t("Skills", "技能"),
        MentionGroup::Mcp => t("MCP", "MCP"),
        MentionGroup::File => t("Files", "文件"),
    };
    div()
        .id(match group {
            MentionGroup::Command => "mention-heading-commands",
            MentionGroup::Skill => "mention-heading-skills",
            MentionGroup::Mcp => "mention-heading-mcp",
            MentionGroup::File => "mention-heading-files",
        })
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(theme.muted_foreground)
        .px_2()
        .pt_1()
        .child(label)
        .into_any_element()
}

fn mention_label(item: &crate::view_model::MentionItem) -> String {
    match item.group {
        MentionGroup::Command => match item.insert.as_str() {
            "/new" => format!("/new · {}", t("new chat", "新对话")),
            "/compact" => format!("/compact · {}", t("compact context", "压缩上下文")),
            "/settings" => format!("/settings · {}", t("settings", "设置")),
            _ => format!("{} · {}", item.insert, item.label),
        },
        MentionGroup::Skill => format!("{} · {}", item.insert, item.label),
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
        JUMP_LATEST_BUTTON_PX, JUMP_LATEST_ROW_PAD, MenuAnchor, MenuPlacement,
        THINKING_MENU_CLEARANCE, model_menu_anchor, thinking_menu_anchor, thinking_menu_placement,
    };

    #[test]
    fn thinking_menu_overlays_instead_of_blanking_a_full_width_row() {
        assert_eq!(thinking_menu_placement(), MenuPlacement::AboveChip);
    }

    #[test]
    fn thinking_menu_clears_the_jump_button() {
        const {
            assert!(
                THINKING_MENU_CLEARANCE >= 88. + JUMP_LATEST_BUTTON_PX + JUMP_LATEST_ROW_PAD + 8.
            );
        }
        assert_eq!(thinking_menu_placement(), MenuPlacement::AboveChip);
        assert_eq!(thinking_menu_anchor(), MenuAnchor::ChipEnd);
    }

    #[test]
    fn composer_menus_anchor_to_their_chips() {
        assert_eq!(thinking_menu_anchor(), MenuAnchor::ChipEnd);
        assert_eq!(model_menu_anchor(), MenuAnchor::ChipStart);
    }
}
