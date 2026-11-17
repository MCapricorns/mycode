use gpui_kit::component::{ActiveTheme as _, theme::Theme};
use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement, Render, Styled, TestAppContext, Window,
    div, px, size,
};

use super::{IconName, action_row, welcome_action};

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
fn welcome_actions_keep_equal_sizes_across_languages_and_wrapping(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for width in [260., 640.] {
        for font_size in ["s", "m", "l", "xl"] {
            for labels in [
                ("Open project", "Just chat"),
                ("Switch project", "Describe a task"),
                ("打开项目", "直接聊聊"),
                ("切换项目", "描述任务"),
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
                    assert_eq!(open.size, chat.size, "{labels:?}/{font_size}/{width}");
                    for button in [open, chat] {
                        assert!(button.left() >= row.left() && button.right() <= row.right());
                        assert!(button.top() >= row.top() && button.bottom() <= row.bottom());
                    }
                    if width < 384. {
                        assert!(
                            chat.top() >= open.bottom(),
                            "wrapped buttons must not overlap"
                        );
                    } else {
                        assert_eq!(open.top(), chat.top());
                    }
                })
                .unwrap();
            }
        }
    }
}
