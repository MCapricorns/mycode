//! Centered project launchpad for an empty conversation.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Focusable as _, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, img, px,
};

use crate::i18n::t;
use crate::ui::{brand, element_id, hover_delete_button, project_label, skin};
use crate::view_model::has_open_folder;
use crate::workspace::Workspace;

const MAX_WELCOME_RECENTS: usize = 6;
pub(super) const CONTENT_WIDTH: f32 = 640.;

/// Welcome scrolls from the top, independently of transcript tail-follow.
pub(super) fn scroll_handle(window: &mut gpui_kit::Window) -> gpui_kit::ScrollHandle {
    window.with_global_id(gpui_kit::ElementId::from("welcome-scroll"), |id, window| {
        window.with_element_state(id, |handle: Option<gpui_kit::ScrollHandle>, _| {
            let handle = handle.unwrap_or_default();
            (handle.clone(), handle)
        })
    })
}

#[must_use]
pub(crate) fn new_task_label() -> &'static str {
    t("New conversation", "新对话")
}

pub(super) fn render_welcome(
    workspace: &Workspace,
    cx: &Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let vm = workspace.vm();
    let has_folder = has_open_folder(vm);
    div()
        .id("welcome")
        .w_full()
        .max_w(px(CONTENT_WIDTH))
        .min_w_0()
        .flex_none()
        .mx_auto()
        .py_6()
        .flex()
        .flex_col()
        .items_center()
        .gap_6()
        .child(
            div()
                .id("welcome-brand")
                .w_full()
                .flex()
                .justify_center()
                .child(img(brand::wordmark_asset()).w(px(240.)).h(px(48.))),
        )
        .child(
            action_row()
                .child(
                    welcome_action(
                        "welcome-open-project",
                        IconName::FolderOpen,
                        if has_folder {
                            t("Switch project", "切换项目")
                        } else {
                            t("Open project", "打开项目")
                        },
                        !has_folder,
                        theme,
                    )
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_open_project_dialog(cx);
                    })),
                )
                .child(
                    welcome_action(
                        "welcome-begin-task",
                        IconName::ArrowRight,
                        if has_folder {
                            t("Describe a task", "描述任务")
                        } else {
                            t("Just chat", "直接聊聊")
                        },
                        has_folder,
                        theme,
                    )
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace
                            .composer()
                            .read(cx)
                            .focus_handle(cx)
                            .focus(window, cx);
                    })),
                ),
        )
        .when(!has_folder && !vm.recents.is_empty(), |this| {
            this.child(
                div()
                    .id("welcome-recents")
                    .mt_2()
                    .w_full()
                    .max_w(px(520.))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .px_3()
                            .text_sm()
                            .font_weight(skin::WEIGHT_STRONG)
                            .text_color(theme.muted_foreground)
                            .child(t("Recent projects", "最近项目")),
                    )
                    .children(vm.recents.iter().take(MAX_WELCOME_RECENTS).map(|project| {
                        div()
                            .id(format!("recent-{}", element_id(project)))
                            .group("recent-row")
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .rounded(skin::radius_control())
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.secondary_hover))
                            .on_click({
                                let project = project.clone();
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_open_recent(&project, cx)
                                })
                            })
                            .child(
                                Icon::new(IconName::Folder)
                                    .small()
                                    .flex_shrink_0()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_sm().truncate().child(project_label(project)))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .truncate()
                                            .child(project.clone()),
                                    ),
                            )
                            .child(hover_delete_button(
                                format!("recent-remove-{}", element_id(project)),
                                IconName::X,
                                "recent-row",
                                {
                                    let project = project.clone();
                                    cx.listener(move |workspace, _, _, cx| {
                                        workspace.on_remove_recent(&project, cx)
                                    })
                                },
                                cx,
                            ))
                    })),
            )
        })
        .into_any_element()
}

fn action_row() -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id("welcome-actions")
        .w_full()
        .flex()
        .flex_wrap()
        .justify_center()
        .items_center()
        .gap_3()
}

fn welcome_action(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    emphasized: bool,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    skin::glass_button(id, emphasized, theme)
        .w(px(192.))
        .max_w_full()
        .flex_shrink_0()
        .h(px(40.))
        .font_weight(skin::WEIGHT_STRONG)
        .child(Icon::new(icon).with_size(px(16.)))
        .child(label)
}

#[cfg(all(test, feature = "test-support"))]
mod layout_tests;
