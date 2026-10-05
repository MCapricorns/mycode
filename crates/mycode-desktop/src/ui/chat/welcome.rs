//! Empty-desk welcome. A title, two outline actions, and recent folders.
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
use crate::workspace::Workspace;

pub(super) fn render_welcome(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
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
                .child(t(
                    "Open a folder, or start a chat.",
                    "打开一个目录,或开始一个对话。",
                )),
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
                .child(
                    welcome_action(
                        "welcome-new-chat",
                        IconName::MessageSquare,
                        t("New chat", "新建对话"),
                        theme,
                    )
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_new_session(cx);
                    })),
                ),
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
