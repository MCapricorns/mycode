//! First working page: the MYCode banner stamp, local scope, and recent projects.
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

const NEW_TASK_LABEL: (&str, &str) = ("New conversation", "新对话");
const SLOGAN: (&str, &str) = ("Local changes. Clear trail.", "每次改动，有迹可循。");
const SUBTITLE: (&str, &str) = (
    "Conversation, code, and a clear trail",
    "对话、编码，每一步都有迹可循",
);
const MAX_WELCOME_RECENTS: usize = 6;
const TAGLINE_NO_FOLDER: (&str, &str) = (
    "Start a conversation or open a project",
    "直接开始对话，或打开一个项目",
);
const TAGLINE_HAS_FOLDER: (&str, &str) = ("Start a conversation", "开始一段对话");

#[must_use]
pub(crate) fn new_task_label() -> &'static str {
    t(NEW_TASK_LABEL.0, NEW_TASK_LABEL.1)
}

fn empty_desk_tagline(has_folder: bool) -> (&'static str, &'static str) {
    if has_folder {
        TAGLINE_HAS_FOLDER
    } else {
        TAGLINE_NO_FOLDER
    }
}

pub(super) fn render_welcome(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents: Vec<String> = workspace
        .vm()
        .recents
        .iter()
        .take(MAX_WELCOME_RECENTS)
        .cloned()
        .collect();
    let has_folder = has_open_folder(workspace.vm());
    let (tagline_en, tagline_zh) = empty_desk_tagline(has_folder);
    div()
        .id("welcome")
        .w_full()
        .flex()
        .flex_col()
        .gap_4()
        .pt_6()
        .pb_4()
        .child(
            div()
                .id("welcome-brand")
                .w_full()
                .py_5()
                .flex()
                .flex_row()
                .items_center()
                .gap_4()
                .child(img("brand/icon.ico").size(px(80.)).flex_shrink_0())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .id("welcome-title")
                                .child(img(brand::wordmark_asset()).w(px(210.)).h(px(42.))),
                        )
                        .child(
                            div()
                                .id("welcome-slogan")
                                .text_sm()
                                .text_color(theme.foreground)
                                .child(t(SLOGAN.0, SLOGAN.1)),
                        )
                        .child(
                            div()
                                .id("welcome-subtitle")
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(t(SUBTITLE.0, SUBTITLE.1)),
                        ),
                ),
        )
        .child(
            div()
                .id("welcome-start")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .id("welcome-tagline")
                                .text_size(px(18.))
                                .font_weight(skin::WEIGHT_STRONG)
                                .child(t(tagline_en, tagline_zh)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(if has_folder {
                                    t(
                                        "Ask a question, describe a change, or drop files and images here.",
                                        "提出问题、描述修改需求，或拖入文件和图片。",
                                    )
                                } else {
                                    t(
                                        "Send a message directly in the default workspace, or open a local folder.",
                                        "可在默认空间直接发送消息对话，或打开本地项目目录。",
                                    )
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .flex_shrink_0()
                        .when(!has_folder, |this| {
                            this.child(
                                welcome_action(
                                    "welcome-open-project",
                                    IconName::FolderOpen,
                                    t("Open project", "打开项目"),
                                    false,
                                    theme,
                                )
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_open_project_dialog(cx);
                                })),
                            )
                        })
                        .child(
                            welcome_action(
                                "welcome-begin-task",
                                IconName::ArrowRight,
                                t("Write a message", "写消息"),
                                has_folder,
                                theme,
                            )
                            .on_click(cx.listener(|workspace, _, window, cx| {
                                workspace.composer().read(cx).focus_handle(cx).focus(window, cx);
                            })),
                        ),
                ),
        )
        .when(!has_folder && !recents.is_empty(), |this| {
            this.child(
                div()
                    .id("welcome-recents")
                    .mt_2()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .pb_2()
                            .child(t("Return to a project", "接着做一个项目")),
                    )
                    .children(recents.iter().map(|project| {
                        div()
                            .id(format!("recent-{}", element_id(project)))
                            .group("recent-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .px_2()
                            .py_2()
                            .border_t_1()
                            .border_color(theme.border)
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
                                    .xsmall()
                                    .flex_shrink_0()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
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

fn welcome_action(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    emphasized: bool,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ink = if emphasized {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    skin::glass_button(id, emphasized, theme)
        .text_color(ink)
        .child(Icon::new(icon).with_size(px(15.)).text_color(ink))
        .child(label)
}
