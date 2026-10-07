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

/// How many dirty files the inspector preview lists before pointing at the
/// full drawer.
const CHANGES_PREVIEW: usize = 6;

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
        .w(px(316.))
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
                .on_click(cx.listener(|workspace, _, _, cx| {
                    cx.stop_propagation();
                    workspace.on_set_inspector(false, workspace.vm().inspector_pinned, cx);
                })),
        )
        .child(
            div()
                .id("inspector-drawer")
                .absolute()
                .top(px(8.))
                .right(px(8.))
                .bottom(px(8.))
                .w(px(320.))
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_1()
                .border_color(super::skin::glass_border(theme))
                .bg(super::skin::glass_sidebar(theme))
                .overflow_hidden()
                .child(inspector_bar(false, cx))
                .child(inspector_body(workspace, cx)),
        )
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
                if docked {
                    workspace.on_set_inspector(true, false, cx);
                } else {
                    workspace.on_set_inspector(true, true, cx);
                }
            }),
            cx,
        ))
        .when(!docked, |this| {
            this.child(super::icon_button(
                "inspector-close",
                IconName::X,
                cx.listener(|workspace, _, _, cx| {
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
    let hidden_count = git.files.len().saturating_sub(shown.len());
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
                    workspace.on_select_git_file(&path, cx);
                    cx.notify();
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
        .when(hidden_count > 0, |this| {
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
        .when(selected.is_some(), |this| {
            this.child(
                div()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .whitespace_normal()
                    .text_color(theme.muted_foreground)
                    .child(workspace.git_diff().to_owned()),
            )
        })
}

/// The full changes drawer: every dirty file plus the selected file's diff.
/// The inspector panel is a short preview; this is where a large working
/// tree stays browsable.
pub(super) fn render_changes_drawer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let git = workspace.git();
    let selected = workspace.git_diff_path();
    let diff = workspace.git_diff().to_owned();
    let files = git.files.clone();
    div()
        .id("changes-drawer-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("changes-drawer-backdrop")
                .absolute()
                .size_full()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_changes_panel(false, cx);
                })),
        )
        .child(
            div()
                .id("changes-drawer")
                .absolute()
                .top(px(44.))
                .right(px(12.))
                .bottom(px(12.))
                .w(px(480.))
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_1()
                .border_color(super::skin::glass_border(theme))
                .bg(theme.popover)
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
                                    workspace.on_select_git_file(&path, cx);
                                    cx.notify();
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
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .whitespace_normal()
                        .text_color(theme.muted_foreground)
                        .child(if diff.is_empty() {
                            t("Pick a file to see its diff.", "选择一个文件查看差异。").to_owned()
                        } else {
                            diff
                        }),
                ),
        )
        .into_any_element()
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
    let last = vm.last_turn.as_ref().filter(|turn| model_of(turn));

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
            let used = live
                .map(|turn| turn.input)
                .or_else(|| last.map(|turn| turn.input))
                .or_else(|| usage.map(|row| row.input))
                .unwrap_or(0);
            this.child(bar_row(
                "context",
                used,
                context_window,
                theme.cyan,
                &format!(
                    "{} / {}",
                    super::compact_count(used),
                    super::compact_count(context_window)
                ),
                theme,
            ))
        })
        .when_some(usage, |this, row| {
            let share = cache_percent(row.cache, row.input);
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
            .when_some(share, |this, share| {
                this.child(stat_line(t("Cache", "缓存"), &format!("{share}%"), theme))
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

fn model_context_window(vm: &crate::view_model::WorkspaceState) -> u64 {
    let shown = vm.selected_model.as_deref();
    let catalog_context = vm.catalog.as_ref().and_then(|catalog| {
        let provider_id = vm.selected_provider.as_deref()?;
        let provider = catalog.provider(provider_id)?;
        provider
            .models
            .iter()
            .find(|model| shown.is_none_or(|id| model.id == id))
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
    figure: &str,
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
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(figure.to_owned()),
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
        .bg(theme.popover)
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
