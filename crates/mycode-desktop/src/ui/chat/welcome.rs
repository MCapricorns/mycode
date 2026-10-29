//! Empty conversation: a prompt to begin and recent projects when none is selected.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Focusable as _, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::i18n::t;
use crate::ui::{element_id, hover_delete_button, project_label, skin};
use crate::view_model::has_open_folder;
use crate::workspace::Workspace;

const NEW_TASK_LABEL: (&str, &str) = ("New conversation", "新对话");
const MAX_WELCOME_RECENTS: usize = 6;
const TAGLINE_NO_FOLDER: (&str, &str) = ("Choose a project", "选择一个项目");
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
        .max_w(px(760.))
        .flex()
        .flex_col()
        .pt_4()
        .pb_4()
        .child(
            div()
                .id("welcome-start")
                .flex()
                .flex_col()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().id("welcome-tagline").text_size(px(22.)).font_weight(skin::WEIGHT_STRONG).child(t(tagline_en, tagline_zh)))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(if has_folder {
                            t("Ask a question, describe a change, or drop files and images here.", "提出问题、描述修改需求，或拖入文件和图片。")
                        } else {
                            t("Open a local folder, then start your conversation.", "打开本地目录，然后开始对话。")
                        })),
                )
                .child(
                    welcome_action(
                        if has_folder { "welcome-begin-task" } else { "welcome-open-project" },
                        if has_folder { IconName::ArrowRight } else { IconName::FolderOpen },
                        if has_folder { t("Write a message", "写消息") } else { t("Open project", "打开项目") },
                        theme,
                    )
                        .flex_shrink_0()
                        .on_click(cx.listener(move |workspace, _, window, cx| {
                            if has_folder {
                                workspace.composer().read(cx).focus_handle(cx).focus(window, cx);
                            } else {
                                workspace.on_open_project_dialog(cx);
                            }
                        })),
                ),
        )
        .when(!has_folder && !recents.is_empty(), |this| {
            this.child(
                div()
                    .id("welcome-recents")
                    .mt_4()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(div().text_xs().text_color(theme.muted_foreground).pb_2().child(t("Return to a project", "接着做一个项目")))
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
                                cx.listener(move |workspace, _, _, cx| workspace.on_open_recent(&project, cx))
                            })
                            .child(Icon::new(IconName::Folder).xsmall().flex_shrink_0().text_color(theme.muted_foreground))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().text_sm().truncate().child(project_label(project)))
                                    .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(project.clone())),
                            )
                            .child(hover_delete_button(
                                format!("recent-remove-{}", element_id(project)), IconName::X, "recent-row",
                                {
                                    let project = project.clone();
                                    cx.listener(move |workspace, _, _, cx| workspace.on_remove_recent(&project, cx))
                                }, cx,
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
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ink = theme.primary_foreground;
    skin::glass_button(id, true, theme)
        .text_color(ink)
        .child(Icon::new(icon).with_size(px(15.)).text_color(ink))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::{TAGLINE_HAS_FOLDER, TAGLINE_NO_FOLDER, empty_desk_tagline};

    #[test]
    fn empty_conversation_starts_with_project_selection_when_unbound() {
        assert_eq!(empty_desk_tagline(false), TAGLINE_NO_FOLDER);
        assert_eq!(empty_desk_tagline(true), TAGLINE_HAS_FOLDER);
    }
}
