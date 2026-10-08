//! Empty-desk welcome. A title, outline actions, and recent folders.
//!
//! With no folder open there is one action: Open folder. Once a folder is
//! open, a second outline action starts a task. Both surfaces share
//! [`new_task_label`], so a rename is one edit.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};

use crate::i18n::t;
use crate::ui::{element_id, hover_delete_button, project_label, skin};
use crate::view_model::has_open_folder;
use crate::workspace::Workspace;

/// Interim label for a new task: the empty-desk action, the sidebar
/// control, and an untitled session row. ui/ux may replace this pair
/// (for example "Ask MYCode").
const NEW_TASK_LABEL: (&str, &str) = ("New task", "新建任务");

const TAGLINE_NO_FOLDER: (&str, &str) = ("Open a folder to get started.", "打开一个目录即可开始。");
const TAGLINE_HAS_FOLDER: (&str, &str) = (
    "Start a task in this workspace.",
    "在此工作区开始一个任务。",
);

/// The new-task label. The empty desk, the sidebar control, and untitled
/// session rows all call this.
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
    let recents = workspace.vm().recents.clone();
    let has_folder = has_open_folder(workspace.vm());
    let (tagline_en, tagline_zh) = empty_desk_tagline(has_folder);
    div()
        .id("welcome")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .py(px(72.))
        .child(
            div()
                .id("welcome-title")
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("MYCode"),
        )
        .child(
            div()
                .id("welcome-tagline")
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t(tagline_en, tagline_zh)),
        )
        .child(
            div()
                .id("welcome-actions")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .mt_2()
                .child(
                    welcome_action(
                        "welcome-open-project",
                        IconName::FolderOpen,
                        t("Open folder", "打开目录"),
                        theme,
                    )
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_open_project_dialog(cx);
                    })),
                )
                .when(has_folder, |actions| {
                    actions.child(
                        welcome_action(
                            "welcome-new-task",
                            IconName::MessageSquare,
                            new_task_label(),
                            theme,
                        )
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_new_session(cx);
                        })),
                    )
                }),
        )
        .when(!recents.is_empty(), |this| {
            this.child(
                div()
                    .id("welcome-recents")
                    .mt_6()
                    .w(px(420.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .px_2()
                            .pb_1()
                            .child(t("Recent", "最近")),
                    )
                    .children(recents.iter().take(6).map(|project| {
                        let project = project.clone();
                        div()
                            .id(format!("recent-{}", element_id(&project)))
                            .group("recent-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .h(px(32.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.secondary_hover))
                            .on_click({
                                let project = project.clone();
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_open_recent(&project, cx);
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
                                    .text_sm()
                                    .truncate()
                                    .child(project_label(&project)),
                            )
                            .child(hover_delete_button(
                                format!("recent-remove-{}", element_id(&project)),
                                IconName::X,
                                "recent-row",
                                {
                                    let project = project.clone();
                                    cx.listener(move |workspace, _, _, cx| {
                                        workspace.on_remove_recent(&project, cx);
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
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ink = theme.foreground;
    // Outline only: neither action takes an accent fill.
    skin::glass_button(id, false, theme)
        .text_color(ink)
        .child(Icon::new(icon).with_size(px(15.)).text_color(ink))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::{NEW_TASK_LABEL, TAGLINE_HAS_FOLDER, TAGLINE_NO_FOLDER, empty_desk_tagline};

    fn rejects_chat_wording(text: &str) {
        assert!(
            !text.to_ascii_lowercase().contains("chat"),
            "empty-desk copy must not say chat: {text}"
        );
        assert!(
            !text.contains("对话"),
            "empty-desk copy must not say chat: {text}"
        );
    }

    #[test]
    fn empty_desk_copy_is_folder_or_task_and_never_chat() {
        for (english, chinese) in [TAGLINE_NO_FOLDER, TAGLINE_HAS_FOLDER, NEW_TASK_LABEL] {
            rejects_chat_wording(english);
            rejects_chat_wording(chinese);
        }
        assert_eq!(empty_desk_tagline(false), TAGLINE_NO_FOLDER);
        assert_eq!(empty_desk_tagline(true), TAGLINE_HAS_FOLDER);
        assert_eq!(TAGLINE_NO_FOLDER.0, "Open a folder to get started.");
        assert_eq!(TAGLINE_HAS_FOLDER.0, "Start a task in this workspace.");
        assert_eq!(NEW_TASK_LABEL.0, "New task");
    }
}
