//! Right panel: live subagent progress, the working tree, and model usage.
//! The full changes drawer also lives here — the inspector list is a preview,
//! the drawer is where a large working tree is browsable.
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use crate::i18n::t;
use crate::view_model::cache_percent;
use crate::workspace::Workspace;

/// How many dirty files the inspector preview lists before the rest are only
/// in the full drawer. View all stays available for any non-empty list so a
/// one-file change can still open the drawer.
const CHANGES_PREVIEW: usize = 6;

/// Docked Details column width, including its outer padding.
pub(super) const INSPECTOR_DOCKED_WIDTH: f32 = 316.;
/// Overlay Details card: 8px from the window edge plus a 320px card.
pub(super) const INSPECTOR_OVERLAY_SPAN: f32 = 328.;
const INSPECTOR_OVERLAY_MARGIN: f32 = 8.;
const INSPECTOR_OVERLAY_WIDTH: f32 = 320.;
const CHANGES_DRAWER_WIDTH: f32 = 480.;
const DIFF_PANEL_WIDTH: f32 = 560.;
const PANEL_GAP: f32 = 12.;

/// Right insets for the changes drawer and the diff panel.
///
/// The diff sits to the left of whichever right-hand surface is open, with a
/// gap, so it does not slide under Details or the review drawer.
pub(crate) fn panel_rights(inspector_span: f32, drawer_open: bool) -> (f32, f32) {
    let drawer_right = if inspector_span > 0. {
        inspector_span + PANEL_GAP
    } else {
        PANEL_GAP
    };
    let diff_right = if drawer_open {
        drawer_right + CHANGES_DRAWER_WIDTH + PANEL_GAP
    } else if inspector_span > 0. {
        inspector_span + PANEL_GAP
    } else {
        PANEL_GAP
    };
    (drawer_right, diff_right)
}

pub(super) fn render_context_panel(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    // Inset card. A full-bleed column sat under the caption buttons and a
    // double-click there both pinned the panel and zoomed the window.
    div()
        .id("context-panel")
        .w(px(INSPECTOR_DOCKED_WIDTH))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .p(px(8.))
        .child(
            div()
                .id("context-panel-card")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(px(12.))
                .border_1()
                .border_color(super::skin::glass_border(theme))
                .bg(super::skin::glass_sidebar(theme))
                .child(inspector_bar(true, cx))
                .child(inspector_body(workspace, cx)),
        )
}

/// Inspector as a right-hand drawer. The conversation keeps the full column.
pub(super) fn render_inspector_drawer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("inspector-layer")
        .absolute()
        .top(px(super::title_bar::BAR_HEIGHT))
        .left_0()
        .right_0()
        .bottom_0()
        .child(
            div()
                .id("inspector-backdrop")
                .absolute()
                .size_full()
                .bg(super::skin::menu_scrim(theme))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    cx.stop_propagation();
                    workspace.on_set_inspector(false, workspace.vm().inspector_pinned, cx);
                })),
        )
        .child(
            div()
                .id("inspector-drawer")
                // The backdrop is a sibling underneath, and a normal hitbox
                // does not block it. Pin used to set `pinned` and the same
                // click then reached the backdrop, which set `open` back to
                // false. The title-bar toggle was the only way to show it
                // again. Occlude drops the backdrop out of that hit test.
                .occlude()
                .absolute()
                .top(px(INSPECTOR_OVERLAY_MARGIN))
                .right(px(INSPECTOR_OVERLAY_MARGIN))
                .bottom(px(INSPECTOR_OVERLAY_MARGIN))
                .w(px(INSPECTOR_OVERLAY_WIDTH))
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_1()
                .border_color(super::skin::glass_border(theme))
                .bg(super::skin::glass_sidebar(theme))
                .overflow_hidden()
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click(|_, _, cx| {
                    cx.stop_propagation();
                })
                .child(inspector_bar(false, cx))
                .child(inspector_body(workspace, cx)),
        )
}

/// Pin keeps the panel open. A docked pin returns to the overlay; an
/// overlay pin stays open and becomes pinned.
pub(crate) fn inspector_flags_after_pin(docked: bool) -> (bool, bool) {
    if docked { (true, false) } else { (true, true) }
}

fn inspector_bar(docked: bool, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(if docked {
            "inspector-bar-docked"
        } else {
            "inspector-bar-drawer"
        })
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .h(px(36.))
        .border_b_1()
        .border_color(super::skin::glass_border(theme))
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t("Details", "详情")),
        )
        .child(super::icon_button(
            if docked {
                "inspector-unpin"
            } else {
                "inspector-pin"
            },
            IconName::Pin,
            cx.listener(move |workspace, _, _, cx| {
                cx.stop_propagation();
                let (open, pinned) = inspector_flags_after_pin(docked);
                workspace.on_set_inspector(open, pinned, cx);
            }),
            cx,
        ))
        .when(!docked, |this| {
            this.child(super::icon_button(
                "inspector-close",
                IconName::X,
                cx.listener(|workspace, _, _, cx| {
                    cx.stop_propagation();
                    workspace.on_set_inspector(false, false, cx);
                }),
                cx,
            ))
        })
}

fn inspector_body(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    div()
        .id("context-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_3()
        .py_3()
        .flex()
        .flex_col()
        .gap_4()
        .when(
            crate::view_model::task_surface_visible(workspace.vm())
                && !workspace.vm().live_jobs.is_empty(),
            |this| this.child(render_subagents(workspace, cx)),
        )
        .child(render_changes(workspace, cx))
        .child(render_model_usage(workspace, cx))
}

fn render_subagents(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let jobs = workspace.vm().live_jobs.clone();
    let summary = if jobs.len() == 1 {
        t("1 running", "1 个运行中").to_owned()
    } else {
        format!("{} {}", jobs.len(), t("running", "个运行中"))
    };
    div()
        .id("subagents")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(div().text_sm().child(t("Subagents", "子代理")))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(summary),
                ),
        )
        .children(jobs.into_iter().enumerate().map(|(index, job)| {
            let call_id = job.call_id.clone();
            let cancel_id = job.call_id.clone();
            let role = if job.role.is_empty() {
                "agent".to_owned()
            } else {
                job.role.clone()
            };
            let title = if job.label.is_empty() {
                role.clone()
            } else {
                job.label.clone()
            };
            let step = if job.step.is_empty() {
                t("starting", "启动中").to_owned()
            } else {
                job.step.clone()
            };
            div()
                .id(format!("subagent-{index}"))
                .flex()
                .flex_col()
                .gap_1()
                .px_1()
                .py(px(4.))
                .rounded(super::skin::radius_control())
                .cursor_pointer()
                .hover(|card| card.bg(super::skin::frost_hover(theme)))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_open_subagent(&call_id, cx);
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(super::lamp(desk.amber))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .truncate()
                                .text_color(theme.foreground)
                                .child(title),
                        )
                        .child(
                            div()
                                .id(format!("subagent-cancel-{index}"))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(18.))
                                .rounded(px(4.))
                                .cursor_pointer()
                                .text_color(theme.muted_foreground)
                                .hover(|this| this.text_color(theme.danger))
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    cx.stop_propagation();
                                    workspace.on_cancel_subagent(&cancel_id, cx);
                                }))
                                .child(Icon::new(IconName::X).xsmall()),
                        ),
                )
                .child(
                    div()
                        .pl(px(15.))
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(step),
                )
        }))
}

fn render_changes(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let git = workspace.git();
    let selected = workspace.git_diff_path();
    let shown: Vec<_> = git.files.iter().take(CHANGES_PREVIEW).collect();
    div()
        .id("changes")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(div().text_sm().child(t("Changes", "改动")))
                .when(!git.files.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} {}", git.files.len(), t("files", "个文件"))),
                    )
                }),
        )
        .when(!git.branch.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(git.branch.clone()),
            )
        })
        .when_some(git.note.clone(), |this, note| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(note),
            )
        })
        .when(git.note.is_none() && git.files.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t("Working tree clean", "工作区无改动")),
            )
        })
        .children(shown.into_iter().enumerate().map(|(index, file)| {
            let path = file.path.clone();
            let open = selected == Some(file.path.as_str());
            div()
                .id(format!("git-file-{index}"))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(28.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(open, |row| row.bg(theme.accent))
                .hover(|row| row.bg(theme.secondary_hover))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    cx.stop_propagation();
                    workspace.on_select_git_file(&path, cx);
                }))
                .child(
                    div()
                        .w(px(18.))
                        .text_xs()
                        .text_color(theme.primary)
                        .child(file.status.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(file.path.clone()),
                )
        }))
        .when(!git.files.is_empty(), |this| {
            this.child(
                div()
                    .id("git-file-more")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(28.))
                    .rounded(px(6.))
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|row| row.bg(theme.secondary_hover).text_color(theme.foreground))
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_toggle_changes_panel(true, cx);
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(format!(
                        "{} {}",
                        t("View all", "查看全部"),
                        git.files.len()
                    )))
                    .child(Icon::new(IconName::ChevronRight).xsmall()),
            )
        })
}

/// The full changes drawer lists the dirty files the snapshot holds. The
/// selected file's diff opens in its own panel. The inspector panel is a
/// short preview; this is where a large working tree stays browsable.
pub(super) fn render_changes_drawer(
    workspace: &mut Workspace,
    inspector_span: f32,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let git = workspace.git();
    let selected = workspace.git_diff_path();
    let diff = workspace.git_diff().to_owned();
    let files = git.files.clone();
    let (drawer_right, _) = panel_rights(inspector_span, true);
    div()
        .id("changes-drawer-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("changes-drawer-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .bottom_0()
                // Leave the Details card uncovered so a click there is not
                // an outside-click on this drawer, and this scrim is not
                // what dismisses Details.
                .right(px(inspector_span))
                .bg(super::skin::menu_scrim(theme))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    cx.stop_propagation();
                    workspace.on_toggle_changes_panel(false, cx);
                })),
        )
        .child(
            div()
                .id("changes-drawer")
                .occlude()
                .absolute()
                .top(px(44.))
                .right(px(drawer_right))
                .bottom(px(12.))
                .w(px(CHANGES_DRAWER_WIDTH))
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_1()
                .border_color(super::skin::glass_border(theme))
                .bg(super::skin::popover(theme))
                .overflow_hidden()
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click(|_, _, cx| {
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(div().flex_1().min_w_0().text_sm().truncate().child(format!(
                            "{}{}",
                            t("Changes", "改动"),
                            if git.branch.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", git.branch)
                            }
                        )))
                        .child(
                            div()
                                .id("changes-drawer-close")
                                .px_2()
                                .py(px(2.))
                                .rounded(px(6.))
                                .text_xs()
                                .cursor_pointer()
                                .text_color(theme.muted_foreground)
                                .hover(|this| this.bg(theme.secondary_hover))
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    cx.stop_propagation();
                                    workspace.on_toggle_changes_panel(false, cx);
                                }))
                                .child(t("Close", "关闭")),
                        ),
                )
                .child(
                    div()
                        .id("changes-drawer-files")
                        .flex_shrink_0()
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .children(files.into_iter().enumerate().map(|(index, file)| {
                            let path = file.path.clone();
                            let open = selected == Some(file.path.as_str());
                            div()
                                .id(format!("changes-drawer-file-{index}"))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .h(px(26.))
                                .rounded(px(6.))
                                .text_xs()
                                .cursor_pointer()
                                .when(open, |row| row.bg(theme.accent))
                                .hover(|row| row.bg(theme.secondary_hover))
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    cx.stop_propagation();
                                    workspace.on_select_git_file(&path, cx);
                                }))
                                .child(
                                    div()
                                        .w(px(20.))
                                        .flex_shrink_0()
                                        .text_color(theme.primary)
                                        .child(file.status.clone()),
                                )
                                .child(div().flex_1().min_w_0().truncate().child(file.path.clone()))
                        })),
                )
                .child(
                    div()
                        .id("changes-drawer-diff")
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if selected.is_none() {
                            t("Pick a file to open its diff.", "选择一个文件以打开差异。")
                                .to_owned()
                        } else if diff.is_empty() {
                            t("Opening the diff panel…", "正在打开差异面板…").to_owned()
                        } else {
                            t(
                                "Diff is open in its own panel.",
                                "差异已在单独的面板中打开。",
                            )
                            .to_owned()
                        }),
                ),
        )
        .into_any_element()
}

/// Dedicated diff panel. File clicks in the changes list and in the review-all
/// drawer both open this, so the patch is not clipped inside the inspector.
pub(super) fn render_diff_panel(
    workspace: &mut Workspace,
    inspector_span: f32,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let path = workspace
        .git_diff_path()
        .unwrap_or(t("Diff", "差异"))
        .to_owned();
    let diff = workspace.git_diff().to_owned();
    let lines = diff_lines(&diff);
    let (_, diff_right) = panel_rights(inspector_span, workspace.vm().changes_panel_open);
    div()
        .id("diff-panel-layer")
        .occlude()
        .absolute()
        .top(px(44.))
        .right(px(diff_right))
        .bottom(px(12.))
        .w(px(DIFF_PANEL_WIDTH))
        .flex()
        .flex_col()
        .rounded(px(12.))
        .border_1()
        .border_color(super::skin::glass_border(theme))
        .bg(super::skin::popover(theme))
        .overflow_hidden()
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .on_click(|_, _, cx| {
            cx.stop_propagation();
        })
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .child(div().flex_1().min_w_0().text_sm().truncate().child(path))
                .child(
                    div()
                        .id("diff-panel-close")
                        .px_2()
                        .py(px(2.))
                        .rounded(px(6.))
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.bg(theme.secondary_hover))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            cx.stop_propagation();
                            workspace.on_close_git_diff_panel(cx);
                        }))
                        .child(t("Close", "关闭")),
                ),
        )
        .child(
            div()
                .id("diff-panel-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .py_2()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .children(lines.into_iter().enumerate().map(|(index, line)| {
                    let (color, bg) = diff_line_paint(&line, theme);
                    div()
                        .id(format!("diff-line-{index}"))
                        .px_3()
                        .text_color(color)
                        .bg(bg)
                        .child(if line.is_empty() {
                            " ".to_owned()
                        } else {
                            line
                        })
                })),
        )
        .into_any_element()
}

fn diff_lines(diff: &str) -> Vec<String> {
    if diff.is_empty() {
        return vec![t("Loading diff…", "正在加载差异…").to_owned()];
    }
    diff.lines().map(str::to_owned).collect()
}

fn diff_line_paint(line: &str, theme: &Theme) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    let added = line.starts_with('+') && !line.starts_with("+++");
    let removed = line.starts_with('-') && !line.starts_with("---");
    if added {
        let ink = vivid(0x3D_FF_9A);
        return (ink, ink.opacity(0.14));
    }
    if removed {
        let ink = vivid(0xFF_5C_6A);
        return (ink, ink.opacity(0.14));
    }
    if line.starts_with("@@") {
        return (theme.primary, theme.transparent);
    }
    (theme.muted_foreground, theme.transparent)
}

fn vivid(hex: u32) -> gpui_kit::Hsla {
    gpui_kit::rgb(hex).into()
}

fn render_model_usage(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let vm = workspace.vm();
    let selected_model = vm
        .selected_model
        .clone()
        .or_else(|| vm.last_turn.as_ref().map(|turn| turn.model.clone()))
        .or_else(|| vm.live_turn.as_ref().map(|turn| turn.model.clone()));
    let model = selected_model.clone();
    let provider = vm.selected_provider.clone().unwrap_or_default();
    let thinking = crate::view_model::selected_reasoning_level(vm);
    let context_window = model_context_window(vm);
    // Prefer the exact provider/model row, then any provider with that model,
    // then the model the last turn actually ran on — the panel should never
    // go blank while a conversation has accounting.
    let exact_key = match (provider.is_empty(), selected_model.as_deref()) {
        (false, Some(model)) => Some(format!("{provider}/{model}")),
        _ => None,
    };
    let usage = exact_key
        .as_deref()
        .and_then(|key| vm.usage_totals.iter().find(|row| row.key == key))
        .or_else(|| {
            selected_model.as_deref().and_then(|model| {
                vm.usage_totals
                    .iter()
                    .find(|row| crate::view_model::usage_key_matches(&row.key, model))
            })
        })
        .or_else(|| {
            vm.last_turn.as_ref().and_then(|turn| {
                vm.usage_totals
                    .iter()
                    .find(|row| crate::view_model::usage_key_matches(&row.key, &turn.model))
            })
        });
    let model_of = |turn: &crate::view_model::TurnStats| {
        selected_model
            .as_deref()
            .is_none_or(|selected| turn.model == selected)
    };
    let live = vm.live_turn.as_ref().filter(|turn| model_of(turn));

    div()
        .id("model-usage")
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_sm().child(t("Model", "模型")))
        .child(
            div().text_sm().whitespace_normal().child(
                model
                    .clone()
                    .unwrap_or_else(|| t("No model selected", "未选择模型").to_owned()),
            ),
        )
        .when(!provider.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(provider),
            )
        })
        .child(stat_line(t("Thinking", "思考"), thinking, theme))
        .when(context_window > 0, |this| {
            // The meter is the latest prompt, kept across an interrupt.
            // Summing every tool round or every turn is what painted 1.4M/1.0M.
            let used = vm.context_used;
            let cached = vm.context_cache;
            let parts = crate::view_model::context_meter_parts(
                used,
                context_window,
                cached,
                t("cached", "缓存"),
            );
            let cache = (!parts.cache.is_empty()).then_some(parts.cache.as_str());
            this.child(bar_row(
                "context",
                used,
                context_window,
                theme.cyan,
                &parts.ratio,
                cache,
                theme,
            ))
        })
        .when_some(usage, |this, row| {
            this.child(stat_line(
                t("Input", "输入"),
                &super::compact_count(row.input),
                theme,
            ))
            .child(stat_line(
                t("Output", "输出"),
                &super::compact_count(row.output),
                theme,
            ))
            .when(row.cache > 0, |this| {
                let value = match cache_percent(row.cache, row.input) {
                    Some(share) => format!("{} · {share}%", super::compact_count(row.cache)),
                    None => super::compact_count(row.cache),
                };
                this.child(stat_line(t("Cache", "缓存"), &value, theme))
            })
            .child(stat_line(
                t("Turns", "轮次"),
                &row.requests.to_string(),
                theme,
            ))
        })
        .when_some(live, |this, turn| {
            this.child(stat_line(
                t("Live", "实时"),
                &format!(
                    "{} {} · {} {}",
                    super::compact_count(turn.input),
                    t("in", "入"),
                    super::compact_count(turn.output),
                    t("out", "出")
                ),
                theme,
            ))
        })
        .when(usage.is_none() && live.is_none(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t("No usage in this session yet.", "本会话暂无用量统计。")),
            )
        })
}

pub(crate) fn model_context_window(vm: &crate::view_model::WorkspaceState) -> u64 {
    let shown = vm.selected_model.as_deref();
    let provider_id = vm.selected_provider.as_deref();
    let base_url = provider_id.and_then(|id| {
        vm.settings.as_ref().and_then(|settings| {
            settings
                .providers
                .iter()
                .find(|provider| provider.id == id)
                .map(|provider| provider.base_url.as_str())
        })
    });
    let catalog_context = vm.catalog.as_ref().and_then(|catalog| {
        let provider_id = provider_id?;
        if let Some(model_id) = shown {
            return catalog
                .model_for_endpoint(provider_id, base_url, model_id)
                .map(|model| model.context)
                .filter(|context| *context > 0);
        }
        catalog
            .provider(provider_id)?
            .models
            .first()
            .map(|model| model.context)
            .filter(|context| *context > 0)
    });
    let override_context = vm.settings.as_ref().and_then(|settings| {
        settings
            .providers
            .iter()
            .find(|provider| Some(provider.id.as_str()) == vm.selected_provider.as_deref())
            .and_then(|provider| provider.context_limit)
            .filter(|context| *context > 0)
    });
    override_context.or(catalog_context).unwrap_or(0)
}

fn stat_line(label: &str, value: &str, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.foreground)
                .child(value.to_owned()),
        )
}

fn bar_row(
    label: &str,
    value: u64,
    total: u64,
    color: gpui_kit::Hsla,
    ratio: &str,
    cache: Option<&str>,
    theme: &Theme,
) -> impl IntoElement {
    let fill = if total == 0 {
        0.0
    } else {
        (value as f32 / total as f32).clamp(0.04, 1.0)
    };
    div()
        .id(format!("bar-{label}"))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t("Context", "上下文")),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(ratio.to_owned()),
                        )
                        .when_some(cache, |this, cache| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(theme.foreground)
                                    .child(format!("· {cache}")),
                            )
                        }),
                ),
        )
        .child(
            div()
                .h(px(4.))
                .w_full()
                .rounded_full()
                .bg(theme.border)
                .child(
                    div()
                        .h_full()
                        .w(px((200.0 * fill).round()))
                        .rounded_full()
                        .bg(color),
                ),
        )
}

fn window_label(text: &str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(text.to_owned())
}

fn window_body(text: String) -> impl IntoElement {
    div().text_sm().whitespace_normal().child(text)
}

/// Small window for one running subagent so its progress is readable.
pub(super) fn render_subagent_window(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let call_id = workspace.vm().subagent_window.clone().unwrap_or_default();
    let job = workspace
        .vm()
        .live_jobs
        .iter()
        .find(|job| job.call_id == call_id)
        .cloned();
    let title = job.as_ref().map(|job| {
        if job.role.is_empty() {
            if job.label.is_empty() {
                t("Subagent", "子代理").to_owned()
            } else {
                job.label.clone()
            }
        } else if job.label.is_empty() {
            job.role.clone()
        } else {
            format!("{} · {}", job.role, job.label)
        }
    });
    let goal = job
        .as_ref()
        .map(|job| job.label.clone())
        .unwrap_or_default();
    let prompt = job
        .as_ref()
        .map(|job| job.prompt.clone())
        .unwrap_or_default();
    let path = job.as_ref().map(|job| job.path.clone()).unwrap_or_default();
    let now = job.as_ref().map(|job| job.step.clone()).unwrap_or_default();
    let log = job.map(|job| job.log).unwrap_or_default();
    let last = log.len().saturating_sub(1);
    div()
        .id("subagent-window-layer")
        .absolute()
        .top(px(44.))
        .right(px(16.))
        .w(px(320.))
        .h(px(280.))
        .flex()
        .flex_col()
        .rounded(px(12.))
        .border_1()
        .border_color(super::skin::glass_border(theme))
        .bg(super::skin::popover(theme))
        .overflow_hidden()
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .child(title.unwrap_or_else(|| t("Subagent", "子代理").to_owned())),
                )
                .child(
                    div()
                        .id("subagent-window-close")
                        .px_2()
                        .py(px(2.))
                        .rounded(px(6.))
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.bg(theme.secondary_hover))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            cx.stop_propagation();
                            workspace.on_open_subagent("", cx);
                        }))
                        .child(t("Close", "关闭")),
                ),
        )
        .child(
            div()
                .id("subagent-window-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(window_label(t("Goal", "目标"), theme))
                .child(window_body(if goal.is_empty() {
                    t("No short goal yet.", "暂无简短目标。").to_owned()
                } else {
                    goal
                }))
                .child(window_label(t("Task", "任务"), theme))
                .child(window_body(if prompt.is_empty() {
                    t("Waiting for the task brief.", "等待任务简报。").to_owned()
                } else {
                    prompt
                }))
                .when(!path.is_empty(), |this| {
                    this.child(window_label(t("Path", "路径"), theme))
                        .child(window_body(path))
                })
                .child(window_label(&format!("{}  {now}", t("Now", "当前")), theme))
                .children(log.into_iter().enumerate().map(|(index, line)| {
                    let current = index == last;
                    div()
                        .id(format!("subagent-log-{index}"))
                        .text_xs()
                        .text_color(if current {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(line)
                })),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::inspector_flags_after_pin;

    #[test]
    fn pinning_leaves_the_panel_open() {
        assert_eq!(inspector_flags_after_pin(false), (true, true));
        assert_eq!(inspector_flags_after_pin(true), (true, false));
    }

    #[test]
    fn the_diff_sits_left_of_details_and_the_drawer() {
        let (drawer, diff) = super::panel_rights(0., false);
        assert_eq!(diff, 12.);
        assert_eq!(drawer, 12.);

        assert_eq!(
            super::INSPECTOR_OVERLAY_SPAN,
            super::INSPECTOR_OVERLAY_MARGIN + super::INSPECTOR_OVERLAY_WIDTH
        );
        let (_, beside_details) = super::panel_rights(super::INSPECTOR_OVERLAY_SPAN, false);
        assert_eq!(beside_details, super::INSPECTOR_OVERLAY_SPAN + 12.);

        let (drawer, beside_drawer) = super::panel_rights(super::INSPECTOR_DOCKED_WIDTH, true);
        assert_eq!(drawer, super::INSPECTOR_DOCKED_WIDTH + 12.);
        assert!(beside_drawer > drawer + 480.);
        assert_eq!(
            beside_drawer,
            super::INSPECTOR_DOCKED_WIDTH + 12. + 480. + 12.
        );
    }
}
