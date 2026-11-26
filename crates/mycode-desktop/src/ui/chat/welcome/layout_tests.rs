use gpui_kit::component::{ActiveTheme as _, theme::Theme};
use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement, Render, Styled, TestAppContext, Window,
    div, px, size,
};

use crate::view_model::{SessionSummary, WorkspaceState};

use super::{
    IconName, action_row, recent_conversations, recent_section, recent_sections, welcome_action,
};

struct ActionsProbe {
    labels: (&'static str, &'static str),
    font_size: &'static str,
}

impl Render for ActionsProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::desk::apply_font_size(Theme::global_mut(cx), self.font_size);
        window.set_rem_size(px(crate::ui::desk::interface_rem_px(self.font_size)));
        let theme = cx.theme();
        div().size_full().child(
            action_row()
                .child(
                    welcome_action("open", IconName::FolderOpen, self.labels.0, true, theme)
                        .test_support(),
                )
                .child(
                    welcome_action("chat", IconName::ArrowRight, self.labels.1, false, theme)
                        .test_support(),
                )
                .test_support(),
        )
    }
}

#[gpui_kit::test]
fn welcome_actions_fit_across_languages_and_font_sizes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for width in [210., 640.] {
        for font_size in ["s", "m", "l", "xl"] {
            for labels in [
                ("Open project", "Chat directly"),
                ("Write a task", "Switch project"),
                ("打开项目", "直接对话"),
                ("输入任务", "切换项目"),
            ] {
                let handle = cx.open_window(size(px(width), px(240.)), move |_, _| ActionsProbe {
                    labels,
                    font_size,
                });
                cx.update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    let row = window.find("welcome-actions").bounds();
                    let open = window.find("open").bounds();
                    let chat = window.find("chat").bounds();
                    assert_eq!(open.size.height, chat.size.height);
                    for button in [open, chat] {
                        assert!(button.left() >= row.left() && button.right() <= row.right());
                        assert!(button.top() >= row.top() && button.bottom() <= row.bottom());
                    }
                    assert!(
                        chat.left() >= open.right() || chat.top() >= open.bottom(),
                        "{labels:?}/{font_size}/{width}: actions overlap"
                    );
                    if width == 640. {
                        assert_eq!(open.top(), chat.top());
                    }
                })
                .unwrap();
            }
        }
    }
}

struct RecentsProbe;

impl Render for RecentsProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div().size_full().child(
            recent_sections()
                .child(recent_section("projects", "Recent projects", theme).test_support())
                .child(
                    recent_section("conversations", "Recent conversations", theme).test_support(),
                ),
        )
    }
}

#[gpui_kit::test]
fn recent_sections_stack_without_overflow_in_a_narrow_pane(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for width in [210., 260., 640.] {
        let handle = cx.open_window(size(px(width), px(240.)), |_, _| RecentsProbe);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let projects = window.find("projects").bounds();
            let conversations = window.find("conversations").bounds();
            assert!(projects.right() <= px(width));
            assert!(conversations.right() <= px(width));
            if width < 640. {
                assert_eq!(projects.left(), conversations.left());
                assert!(conversations.top() >= projects.bottom());
            } else {
                assert_eq!(projects.top(), conversations.top());
                assert!(conversations.left() >= projects.right());
            }
        })
        .unwrap();
    }
}

#[test]
fn recent_conversations_follow_the_open_project_and_skip_unusable_rows() {
    let session = |id: &str, event_count, active, corrupt| SessionSummary {
        session_id: id.to_owned(),
        root_branch_id: "branch".to_owned(),
        title: id.to_owned(),
        updated_at_ms: None,
        event_count,
        active,
        corrupt,
    };
    let mut state = WorkspaceState {
        sessions: vec![
            session("active", 3, true, false),
            session("damaged", 2, false, true),
            session("empty", 0, false, false),
            session("first", 2, false, false),
            session("second", 2, false, false),
            session("third", 2, false, false),
            session("fourth", 2, false, false),
            session("fifth", 2, false, false),
        ],
        session_projects: vec![
            ("first".to_owned(), "/project-a".to_owned()),
            ("second".to_owned(), "/project-b".to_owned()),
            ("fifth".to_owned(), "/project-a".to_owned()),
        ],
        ..WorkspaceState::default()
    };
    let recent_ids = |state: &WorkspaceState| {
        recent_conversations(state)
            .into_iter()
            .map(|(summary, _)| summary.session_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(recent_ids(&state), ["first", "second", "third", "fourth"]);

    state.project_dir = Some("/project-a".to_owned());
    assert_eq!(recent_ids(&state), ["first", "fifth"]);
}
