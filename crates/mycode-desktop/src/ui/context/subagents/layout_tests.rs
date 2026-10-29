use super::*;
use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
use gpui_kit::{AppContext as _, Render, TestAppContext, size};
use std::cell::RefCell;
use std::rc::Rc;

struct PreviousResultProbe(Rc<RefCell<WorkspaceState>>);
impl Render for PreviousResultProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.0.borrow();
        let session = state.subagent_session().unwrap();
        let workflow = &session.workflows[0];
        let theme = cx.theme();
        let previous = previous_workflow_runs(workflow, session);
        div()
            .size_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .id("current-status")
                    .child(workflow_status_text(workflow))
                    .test_support(),
            )
            .child(
                detail_section(
                    "current-acceptance",
                    "Independent acceptance",
                    &acceptance_text(workflow),
                    theme,
                )
                .test_support(),
            )
            .child(
                div()
                    .id("previous-results")
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child("Previous results")
                    .children(previous.into_iter().map(|run| {
                        let run_id = run.id.clone();
                        previous_result_button(run, theme).on_click(cx.listener(
                            move |probe, _, _, cx| {
                                crate::view_model::reduce(
                                    &mut probe.0.borrow_mut(),
                                    FrontendAction::SubagentWindowChanged(Some(run_id.clone())),
                                );
                                cx.notify();
                            },
                        ))
                    }))
                    .test_support(),
            )
            .when_some(
                state
                    .subagent_window
                    .as_deref()
                    .and_then(|id| session.run(id)),
                |this, selected| {
                    this.child(
                        detail_section(
                            "selected-previous-result",
                            "Result",
                            selected.result.as_deref().unwrap(),
                            theme,
                        )
                        .test_support(),
                    )
                },
            )
    }
}

#[gpui_kit::test]
fn previous_workflow_result_is_visible_and_opens_the_retained_run(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for width in [390., 700., 1440.] {
        let state = Rc::new(RefCell::new(super::controls_tests::reworked_state()));
        let probe = Rc::clone(&state);
        let handle = cx.open_window(size(px(width), px(600.)), move |_, _| {
            PreviousResultProbe(probe)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let history = window.find("previous-results").bounds();
            assert!(history.left() >= px(0.) && history.right() <= px(width));
            let button = window.find("workflow-previous-display-run-id");
            assert!(button.visible());
            assert!(button.bounds().size.height >= px(44.));
            assert!(window.find("current-acceptance").bounds().top() < history.top());
            window.click("workflow-previous-display-run-id", cx);
            assert_eq!(
                state.borrow().subagent_window.as_deref(),
                Some("display-run-id")
            );
            window.render_frame(cx);
            assert!(window.find("selected-previous-result").visible());
            assert_eq!(
                state.borrow().subagent_session().unwrap().workflows[0].revision,
                1
            );
        })
        .unwrap();
    }
}

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
            activity: "Acceptance found a mismatch in the integrated workspace".into(),
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
            assert!(
                detail.left() >= px(0.) && detail.right() <= px(width),
                "detail {detail:?}"
            );
            assert!(
                detail.top() >= px(0.) && detail.bottom() <= px(height),
                "detail {detail:?}"
            );
            assert!(
                header.size.height >= px(39.),
                "header cannot shrink {header:?}"
            );
            assert!(
                row.left() >= detail.left() && row.right() <= detail.right(),
                "row {row:?}"
            );
            assert!(
                result.top() < path.top() && path.top() < prompt.top(),
                "result must precede lengthy details"
            );
            assert!(
                path.right() <= detail.right(),
                "long path must wrap inside detail {path:?}"
            );
            assert!(
                path.size.height > px(40.),
                "long machine path must occupy several lines {path:?}"
            );
            assert!(
                prompt.size.height > detail.size.height,
                "full brief must scroll, not shrink {prompt:?}"
            );
        })
        .unwrap();
    }
}
