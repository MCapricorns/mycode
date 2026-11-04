use super::*;
use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
use gpui_kit::{AppContext as _, Render, TestAppContext, size};

struct DetailProbe {
    width: f32,
    height: f32,
}
impl Render for DetailProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (right, width) = subagent_window_bounds(self.width, 320., true, true);
        let top = 48.;
        let path = format!(
            "C:/workspace/{}/result.patch",
            "a_very_long_directory_name".repeat(14)
        );
        let row = AgentRow {
            id: "run".into(),
            title: "Audit cancellation and retained results under window resizing".into(),
            role: "sentinel".into(),
            status: t("Failed", "失败").into(),
            tone: StatusTone::Error,
            elapsed: "3m 05s".into(),
            activity: "Found a mismatch in the workspace".into(),
            cancel_id: None,
        };
        div().size_full().child(div().id("detail-probe").absolute().top(px(top)).right(px(right))
            .bottom(px(detail_bottom_inset(self.height, top))).w(px(width)).flex().flex_col().overflow_hidden().test_support()
            .child(div().id("detail-probe-header").h(px(40.)).flex_shrink_0().child("Subagent details").test_support())
            .child(div().id("detail-probe-scroll").flex_1().min_h_0().overflow_y_scroll().p_3().flex().flex_col().gap_4().test_support()
                .child(div().id("detail-probe-row").min_w_0().child(Button::new("probe-open-run").ghost().h_auto().w_full().min_w_0().px_2().py_2().accessibility_label("Open failed run details").child(row_content(&row, theme)).on_click(|_, _, _| {})).test_support())
                .child(detail_section("probe-result", "Result", "The result stays above the optional task brief and activity history.", theme).test_support())
                .child(detail_section("probe-path", "Artifact path", &path, theme).test_support())
                .child(detail_section("probe-prompt", "Task brief", &"A long brief should scroll below the result. ".repeat(100), theme).test_support())))
    }
}

#[gpui_kit::test]
fn result_and_long_paths_fit_narrow_short_and_resized_windows(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for (width, height) in [(1440., 900.), (700., 600.), (390., 420.)] {
        let handle = cx.open_window(size(px(width), px(height)), move |_, _| DetailProbe {
            width,
            height,
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let detail = window.find("detail-probe").bounds();
            let header = window.find("detail-probe-header").bounds();
            let row = window.find("detail-probe-row").bounds();
            let result = window.find("probe-result").bounds();
            let path = window.find("probe-path").bounds();
            let prompt = window.find("probe-prompt").bounds();
            assert!(detail.left() >= px(0.) && detail.right() <= px(width));
            assert!(detail.top() >= px(0.) && detail.bottom() <= px(height));
            assert!(header.size.height >= px(39.));
            assert!(row.left() >= detail.left() && row.right() <= detail.right());
            assert!(result.top() < path.top() && path.top() < prompt.top());
            assert!(path.right() <= detail.right());
            assert!(path.size.height > px(40.));
            assert!(prompt.size.height > detail.size.height);
        })
        .unwrap();
    }
}
