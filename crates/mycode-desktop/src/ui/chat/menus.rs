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
use crate::view_model::{MentionKind, selected_reasoning_level};
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
        .justify_end()
        .child(crate::ui::model_picker::render_model_picker(
            workspace,
            window,
            crate::ui::model_picker::ModelPickerTarget::Session,
            cx,
        ))
        .into_any_element()
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
    div()
        .id("thinking-menu-layer")
        .w_full()
        .px_4()
        .pb_1()
        .flex()
        .flex_row()
        .justify_end()
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
        MentionKind::File => t("FILES", "文件"),
        MentionKind::Command => t("COMMANDS", "命令"),
    };
    div()
        .id("mention-layer")
        .w_full()
        .px_4()
        .pb_1()
        .child(
            popover_panel("mention-menu", theme)
                .w_full()
                .max_h(px(300.))
                .overflow_y_scroll()
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .opacity(0.6)
                        .px_2()
                        .pt_1()
                        .child(heading),
                )
                .children(mention.items.into_iter().map(|(insert, display)| {
                    let row_id = format!("mention-{insert}");
                    menu_row(
                        row_id,
                        display,
                        false,
                        cx.listener(move |workspace, _, window, cx| {
                            workspace.on_accept_mention(insert.clone(), window, cx);
                        }),
                        cx.theme(),
                    )
                })),
        )
        .into_any_element()
}
