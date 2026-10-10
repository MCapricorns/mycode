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
use crate::ui::skin::{self, popover_panel};
use crate::view_model::{
    MentionGroup, MentionKind, preferred_slash_index, selected_reasoning_level,
};
use crate::workspace::Workspace;

/// Fixed row height that keeps the menu rows visually uniform.
const MENU_ROW_HEIGHT: gpui_kit::Pixels = px(30.);

/// The model picker, docked in-flow above the composer. Same provider step
/// and search list the settings page uses.
pub(super) fn render_model_menu(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    div()
        .id("model-menu-layer")
        .w_full()
        .px_4()
        .pb_1()
        .flex()
        .flex_row()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(super::COLUMN_MAX)
                .flex()
                .flex_row()
                .justify_end()
                .child(crate::ui::model_picker::render_model_picker(
                    workspace,
                    window,
                    crate::ui::model_picker::ModelPickerTarget::Session,
                    cx,
                )),
        )
        .into_any_element()
}

/// Where the thinking menu sits. An in-flow full-width row hides the
/// transcript under a blank band; the overlay only covers its own panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuPlacement {
    /// Full-width row between the transcript and the composer.
    ///
    /// Not used. A row of this shape blanked the transcript.
    #[allow(dead_code)]
    InFlowRow,
    /// Bottom-right of the transcript. The transcript keeps its height.
    BottomRightOverlay,
}

/// The thinking menu overlays the transcript instead of inserting a row.
#[must_use]
pub(super) fn thinking_menu_placement() -> MenuPlacement {
    MenuPlacement::BottomRightOverlay
}

/// Thinking effort as its own short list, opened from the composer button.
pub(super) fn render_thinking_menu(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let levels = crate::view_model::selected_reasoning_levels(workspace.vm());
    let selected = selected_reasoning_level(workspace.vm()).to_owned();
    let weak = cx.weak_entity();
    let _placement = thinking_menu_placement();
    div()
        .id("thinking-menu-layer")
        .absolute()
        .bottom(px(8.))
        .right(px(24.))
        .occlude()
        .child(
            popover_panel("thinking-menu", theme)
                .w(px(220.))
                .flex_none()
                .p_1()
                .flex()
                .flex_col()
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

#[cfg(test)]
mod tests {
    use super::{MenuPlacement, thinking_menu_placement};

    #[test]
    fn thinking_menu_overlays_instead_of_blanking_a_full_width_row() {
        assert_eq!(thinking_menu_placement(), MenuPlacement::BottomRightOverlay);
    }
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

pub(crate) fn reasoning_row_label(level: &str) -> String {
    match level {
        "default" => t("Default \u{b7} provider", "默认 \u{b7} 跟随服务商").to_owned(),
        "off" => t("Off", "关闭").to_owned(),
        "on" => t("On", "开启").to_owned(),
        "minimal" => t("Minimal", "极简").to_owned(),
        "low" => t("Low \u{b7} brief", "低 \u{b7} 简短").to_owned(),
        "medium" => t("Medium \u{b7} balanced", "中 \u{b7} 均衡").to_owned(),
        "high" => t("High \u{b7} deep", "高 \u{b7} 深入").to_owned(),
        "xhigh" => t("Extra high", "超高").to_owned(),
        "max" => t("Max", "最高").to_owned(),
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
            div().w_full().max_w(super::COLUMN_MAX).child(
                popover_panel("mention-menu", theme)
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
