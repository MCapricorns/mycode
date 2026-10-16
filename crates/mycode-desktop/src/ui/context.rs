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
    Window, div, px, relative,
};

use crate::i18n::t;
use crate::workspace::Workspace;

/// How many dirty files the inspector preview lists before the rest are only
/// in the full drawer. View all stays available for any non-empty list so a
/// one-file change can still open the drawer.
const CHANGES_PREVIEW: usize = 6;

/// Docked Details column width, including its outer padding.
pub(super) const INSPECTOR_DOCKED_WIDTH: f32 = 316.;
/// Overlay Details card: 8px from the window edge plus a 320px card.
pub(super) const INSPECTOR_OVERLAY_SPAN: f32 = 328.;

/// Right inset for the chat column when Details floats over the conversation.
///
/// A docked Details column already occupies its own width in the row, so the
/// chat pane must not reserve that strip a second time. When the card floats,
/// the transcript and composer stop at its left edge instead of running
/// underneath it. `chat_width` is the pane beside the sidebar; the reserve
/// never eats the last [`MIN_CHAT_BESIDE_DETAILS`] pixels, so a narrow window
/// still has a usable composer.
pub(crate) const MIN_CHAT_BESIDE_DETAILS: f32 = 420.;

pub(crate) fn chat_right_reserve(details_open: bool, details_docked: bool, chat_width: f32) -> f32 {
    if !(details_open && !details_docked) {
        return 0.;
    }
    let room = (chat_width - MIN_CHAT_BESIDE_DETAILS).max(0.);
    INSPECTOR_OVERLAY_SPAN.min(room)
}

const INSPECTOR_OVERLAY_MARGIN: f32 = 8.;
const INSPECTOR_OVERLAY_WIDTH: f32 = 320.;
const CHANGES_DRAWER_WIDTH: f32 = 480.;
const DIFF_PANEL_WIDTH: f32 = 560.;
const PANEL_GAP: f32 = 12.;
const PANEL_MIN_WIDTH: f32 = 280.;

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

/// Responsive right inset and width for the changes drawer and diff panel so
/// neither panel slides past the left edge of narrower windows.
#[must_use]
pub(crate) fn responsive_panel_bounds(
    viewport_width: f32,
    inspector_span: f32,
    drawer_open: bool,
) -> ((f32, f32), (f32, f32)) {
    let min_left = PANEL_GAP;
    let max_span = (viewport_width - min_left * 2.).max(PANEL_MIN_WIDTH);
    let base_right = panel_rights(inspector_span, drawer_open).0;

    let drawer_avail = viewport_width - base_right - min_left;
    let (drawer_right, drawer_width) = if drawer_avail >= PANEL_MIN_WIDTH {
        (base_right, CHANGES_DRAWER_WIDTH.min(drawer_avail))
    } else {
        let width = CHANGES_DRAWER_WIDTH.min(max_span);
        let right = (viewport_width - width - min_left).max(PANEL_GAP);
        (right, width)
    };

    let preferred_diff_right = if drawer_open {
        drawer_right + drawer_width + PANEL_GAP
    } else {
        base_right
    };
    let diff_avail = viewport_width - preferred_diff_right - min_left;
    let (diff_right, diff_width) = if diff_avail >= DIFF_PANEL_WIDTH {
        (preferred_diff_right, DIFF_PANEL_WIDTH)
    } else if diff_avail >= 360. {
        (preferred_diff_right, diff_avail)
    } else {
        // When side-by-side stacking would push the diff panel off-screen or
        // crush it too narrow, anchor it inside the viewport over the drawer.
        let fallback_avail = viewport_width - base_right - min_left;
        if fallback_avail >= 360. {
            (base_right, DIFF_PANEL_WIDTH.min(fallback_avail))
        } else {
            let width = DIFF_PANEL_WIDTH.min(max_span);
            let right = (viewport_width - width - min_left).max(PANEL_GAP);
            (right, width)
        }
    };

    ((drawer_right, drawer_width), (diff_right, diff_width))
}

const SUBAGENT_WINDOW_WIDTH: f32 = 320.;

/// Right inset and width for the subagent window: it stacks left of
/// Details, the changes drawer, and the diff panel, and clamps inside the
/// viewport when every surface is open at once.
#[must_use]
pub(crate) fn subagent_window_bounds(
    viewport_width: f32,
    inspector_span: f32,
    drawer_open: bool,
    diff_open: bool,
) -> (f32, f32) {
    let ((drawer_right, drawer_width), (diff_right, diff_width)) =
        responsive_panel_bounds(viewport_width, inspector_span, drawer_open);
    let preferred = if diff_open {
        diff_right + diff_width + PANEL_GAP
    } else if drawer_open {
        drawer_right + drawer_width + PANEL_GAP
    } else {
        // The strip the diff panel would occupy: clear of Details.
        diff_right
    };
    let right = if preferred + SUBAGENT_WINDOW_WIDTH + PANEL_GAP <= viewport_width {
        preferred
    } else {
        (viewport_width - SUBAGENT_WINDOW_WIDTH - PANEL_GAP).max(PANEL_GAP)
    };
    (right, SUBAGENT_WINDOW_WIDTH)
}

/// Proportional fill fraction in `0.0..=1.0` for the context usage bar.
#[must_use]
pub(crate) fn context_bar_fraction(value: u64, total: u64) -> f32 {
    if total == 0 || value == 0 {
        0.0
    } else {
        (value as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// Semantic color for a git porcelain status code (`A`, `M`, `D`, `??`, etc.).
#[must_use]
pub(crate) fn git_status_color(status: &str, theme: &Theme) -> gpui_kit::Hsla {
    let trimmed = status.trim();
    if trimmed == "??" {
        super::desk::Desk::of(theme).faint
    } else if trimmed.contains('D') {
        theme.danger
    } else if trimmed.contains('A') {
        theme.success
    } else if trimmed.contains('M') || trimmed.contains('R') || trimmed.contains('C') {
        theme.warning
    } else {
        theme.primary
    }
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
                .rounded(super::skin::radius_card())
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
                .rounded(super::skin::radius_card())
                .border_1()
                .border_color(super::skin::border_strong(theme))
                .bg(super::skin::glass_sidebar(theme))
                .shadow(super::skin::elevation_shadow(theme))
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

fn section_heading(text: &str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(theme.muted_foreground)
        .child(text.to_uppercase())
}

fn render_subagents(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let jobs = workspace.vm().live_jobs.clone();
    let capped = crate::view_model::capped_artisan_count(&jobs);
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
                .child(section_heading(t("Subagents", "子代理"), theme))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(summary),
                ),
        )
        .when(capped > 0, |this| {
            this.child(
                div()
                    .id("subagents-cap")
                    .text_xs()
                    .whitespace_normal()
                    .px_2()
                    .py_1()
                    .rounded(super::skin::radius_control())
                    .bg(theme.warning.opacity(0.18))
                    .text_color(theme.warning_foreground)
                    .child(crate::view_model::capped_artisan_status(capped)),
            )
        })
        .children(jobs.into_iter().enumerate().map(|(index, job)| {
            let call_id = job.call_id.clone();
            let cancel_id = job.call_id.clone();
            let title = job.title();
            let step = job.step_label();
            let warned = job.capped;
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
                        .child(super::lamp(if warned { theme.warning } else { desk.amber }))
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
                                .px_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .h(px(18.))
                                .rounded(px(4.))
                                .cursor_pointer()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .hover(|this| this.text_color(theme.danger))
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    cx.stop_propagation();
                                    workspace.on_cancel_subagent(&cancel_id, cx);
                                }))
                                .child(t("Cancel", "取消")),
                        ),
                )
                .child(
                    div()
                        .pl(px(15.))
                        .text_xs()
                        .truncate()
                        .text_color(if warned {
                            theme.warning_foreground
                        } else {
                            theme.muted_foreground
                        })
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
                .child(section_heading(t("Changes", "改动"), theme))
                .when(!git.files.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{}{} {}",
                                git.files.len(),
                                if git.stopped_early { "+" } else { "" },
                                t("files", "个文件")
                            )),
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
            let badge_color = git_status_color(&file.status, theme);
            div()
                .id(format!("git-file-{index}"))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(28.))
                .rounded(super::skin::radius_xs())
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
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(badge_color)
                        .child(file.status.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(git_file_label(file)),
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
                        "{} {}{}",
                        t("View all", "查看全部"),
                        git.files.len(),
                        if git.stopped_early { "+" } else { "" }
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
    window: &Window,
    inspector_span: f32,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let git = workspace.git();
    let selected = workspace.git_diff_path();
    let diff = workspace.git_diff().to_owned();
    let files = git.files.clone();
    let stopped_early = git.stopped_early;
    let viewport_width = f32::from(window.viewport_size().width);
    let ((drawer_right, drawer_width), _) =
        responsive_panel_bounds(viewport_width, inspector_span, true);
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
                .top(px(super::title_bar::BAR_HEIGHT + 8.))
                .right(px(drawer_right))
                .bottom(px(12.))
                .w(px(drawer_width))
                .flex()
                .flex_col()
                .rounded(super::skin::radius_card())
                .border_1()
                .border_color(super::skin::border_strong(theme))
                .bg(super::skin::popover(theme))
                .shadow(super::skin::elevation_shadow(theme))
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
                            let badge_color = git_status_color(&file.status, theme);
                            div()
                                .id(format!("changes-drawer-file-{index}"))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .h(px(26.))
                                .rounded(super::skin::radius_xs())
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
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .text_color(badge_color)
                                        .child(file.status.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(git_file_label(&file)),
                                )
                        }))
                        .when(stopped_early, |list| {
                            list.child(
                                div()
                                    .px_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(t(
                                        "More changes are not listed.",
                                        "还有更多改动未列出。",
                                    )),
                            )
                        }),
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
    window: &Window,
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
    let viewport_width = f32::from(window.viewport_size().width);
    let (_, (diff_right, diff_width)) = responsive_panel_bounds(
        viewport_width,
        inspector_span,
        workspace.vm().changes_panel_open,
    );
    div()
        .id("diff-panel-layer")
        .occlude()
        .absolute()
        .top(px(super::title_bar::BAR_HEIGHT + 8.))
        .right(px(diff_right))
        .bottom(px(12.))
        .w(px(diff_width))
        .flex()
        .flex_col()
        .rounded(super::skin::radius_card())
        .border_1()
        .border_color(super::skin::border_strong(theme))
        .bg(super::skin::popover(theme))
        .shadow(super::skin::elevation_shadow(theme))
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
                .children(numbered_diff_rows(&lines).into_iter().enumerate().map(
                    |(index, row)| {
                        let (color, bg) = diff_line_paint(&row.text, theme);
                        div()
                            .id(format!("diff-line-{index}"))
                            .flex()
                            .flex_row()
                            .px_2()
                            .text_color(color)
                            .bg(bg)
                            .child(
                                div()
                                    .w(px(36.))
                                    .flex_shrink_0()
                                    .text_right()
                                    .text_color(theme.muted_foreground)
                                    .child(row.number),
                            )
                            .child(div().min_w_0().pl_2().child(if row.text.is_empty() {
                                " ".to_owned()
                            } else {
                                row.text
                            }))
                    },
                )),
        )
        .into_any_element()
}

struct NumberedDiffRow {
    number: String,
    text: String,
}

fn numbered_diff_rows(lines: &[String]) -> Vec<NumberedDiffRow> {
    let mut old_n = 1u32;
    let mut new_n = 1u32;
    let mut rows = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some((old, new)) = hunk_numbers(line) {
            old_n = old;
            new_n = new;
            rows.push(NumberedDiffRow {
                number: String::new(),
                text: line.clone(),
            });
            continue;
        }
        let added = line.starts_with('+') && !line.starts_with("+++");
        let removed = line.starts_with('-') && !line.starts_with("---");
        let number = if added {
            let value = new_n.to_string();
            new_n += 1;
            value
        } else if removed {
            let value = old_n.to_string();
            old_n += 1;
            value
        } else if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("+++")
            || line.starts_with("---")
        {
            String::new()
        } else {
            let value = new_n.to_string();
            old_n += 1;
            new_n += 1;
            value
        };
        rows.push(NumberedDiffRow {
            number,
            text: line.clone(),
        });
    }
    rows
}

fn hunk_numbers(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.split_once(' ').map(|(head, _)| head).unwrap_or(rest);
    let old = old.strip_prefix('-')?.split(',').next()?;
    let new = new.strip_prefix('+')?.split(',').next()?;
    Some((old.parse().ok()?, new.parse().ok()?))
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
        let ink = crate::ui::desk::diff_ink(theme, true);
        return (ink, crate::ui::desk::diff_wash(ink));
    }
    if removed {
        let ink = crate::ui::desk::diff_ink(theme, false);
        return (ink, crate::ui::desk::diff_wash(ink));
    }
    if line.starts_with("@@") {
        return (theme.primary, theme.transparent);
    }
    (theme.muted_foreground, theme.transparent)
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
    let model_of = |turn: &crate::view_model::TurnStats| {
        selected_model
            .as_deref()
            .is_none_or(|selected| turn.model == selected)
    };
    // Copied before `turn_row` borrows `vm`. The warning outlives the turn
    // row: it is the prompt after a rewrite, not the summed hit.
    let cache_alert = vm.cache_alert;
    // Usage accounting is live-only: the running turn reports through the
    // live meter, and a finished turn keeps its sums as the last-turn row.
    // These are totals over every request of the turn (one per tool round),
    // so they exceed the context meter, which is the latest prompt alone;
    // the labels and the request count say so.
    let turn_row = vm
        .live_turn
        .as_ref()
        .filter(|turn| model_of(turn))
        .map(|turn| (t("This turn, total", "本轮合计"), turn))
        .or_else(|| {
            vm.last_turn
                .as_ref()
                .filter(|turn| model_of(turn))
                .map(|turn| (t("Last turn, total", "上一轮合计"), turn))
        });
    let show_turn = turn_row.is_some();

    div()
        .id("model-usage")
        .flex()
        .flex_col()
        .gap_3()
        .child(section_heading(t("Model", "模型"), theme))
        .child(
            div()
                .text_sm()
                .whitespace_normal()
                .child(model.unwrap_or_else(|| t("No model selected", "未选择模型").to_owned())),
        )
        .when(!provider.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(provider),
            )
        })
        .child(stat_line(
            t("Thinking", "思考"),
            &super::chat::reasoning_row_label(&thinking),
            theme.foreground,
            theme,
        ))
        .when(context_window > 0, |this| {
            // The meter is the latest prompt, kept across an interrupt.
            // Summing every tool round or every turn is what painted 1.4M/1.0M.
            // The pieces mirror the composer chip one-for-one (ratio · hit ·
            // cache) so the two never read as disagreeing numbers.
            let used = vm.context_used;
            let cached = vm.context_cache;
            let parts = crate::view_model::context_meter_parts(
                used,
                context_window,
                cached,
                t("cached", "缓存"),
                t("cache hit", "缓存命中"),
            );
            this.child(bar_row(
                "context",
                used,
                context_window,
                theme.cyan,
                &parts,
                theme,
            ))
        })
        .when_some(turn_row, |this, (label, turn)| {
            let hit = crate::view_model::turn_cache_hit(turn);
            this.child(stat_line(
                label,
                &format!(
                    "{} {} · {} {} · {} {}",
                    turn.requests,
                    t("requests", "次请求"),
                    super::compact_count(turn.input),
                    t("in", "入"),
                    super::compact_count(turn.output),
                    t("out", "出")
                ),
                theme.foreground,
                theme,
            ))
            .when_some(hit, |row, percent| {
                row.child(stat_line(
                    t("Turn cache hit", "本轮缓存命中"),
                    &format!("{percent}%"),
                    theme.foreground,
                    theme,
                ))
            })
        })
        .when_some(cache_alert, |this, alert| {
            this.child(cache_collapse_note(
                crate::view_model::cache_alert_text(&alert),
                theme,
            ))
        })
        .when(!show_turn && cache_alert.is_none(), |this| {
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

fn stat_line(
    label: &str,
    value: &str,
    value_color: gpui_kit::Hsla,
    theme: &Theme,
) -> impl IntoElement {
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
                .text_color(value_color)
                .child(value.to_owned()),
        )
}

/// Persistent Details warning after compaction or a steer drops the cache.
fn cache_collapse_note(text: String, theme: &Theme) -> impl IntoElement {
    div()
        .px_2()
        .py_1()
        .rounded(super::skin::radius_sm())
        .bg(theme.warning.opacity(0.18))
        .text_color(theme.warning_foreground)
        .text_xs()
        .whitespace_normal()
        .child(text)
}

/// The context bar row. The right-hand pieces come from the same
/// `context_meter_parts` the composer chip renders, in the same order:
/// ratio, then hit percent, then the cached count.
fn bar_row(
    label: &str,
    value: u64,
    total: u64,
    color: gpui_kit::Hsla,
    parts: &crate::view_model::ContextMeterParts,
    theme: &Theme,
) -> impl IntoElement {
    let fill = context_bar_fraction(value, total);
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
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(parts.ratio.clone()),
                        )
                        .when(!parts.hit.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(theme.foreground)
                                    .child(format!("· {}", parts.hit)),
                            )
                        })
                        .when(!parts.cache.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(theme.foreground)
                                    .child(format!("· {}", parts.cache)),
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
                .overflow_hidden()
                .child(div().h_full().w(relative(fill)).rounded_full().bg(color)),
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

/// Tall window for one running subagent so its progress is readable.
pub(super) fn render_subagent_window(
    workspace: &mut Workspace,
    window: &Window,
    inspector_span: f32,
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
    let viewport_width = f32::from(window.viewport_size().width);
    let (window_right, window_width) = subagent_window_bounds(
        viewport_width,
        inspector_span,
        workspace.vm().changes_panel_open,
        workspace.git_diff_panel_open(),
    );
    div()
        .id("subagent-window-layer")
        .occlude()
        .absolute()
        .top(px(super::title_bar::BAR_HEIGHT + 8.))
        .right(px(window_right))
        .bottom(px(12.))
        .w(px(window_width))
        .flex()
        .flex_col()
        .rounded(super::skin::radius_card())
        .border_1()
        .border_color(super::skin::border_strong(theme))
        .bg(super::skin::popover(theme))
        .shadow(super::skin::elevation_shadow(theme))
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

fn git_file_label(file: &crate::git_status::GitFile) -> String {
    match &file.previous {
        Some(previous) => format!("{previous} -> {}", file.path),
        None => file.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::inspector_flags_after_pin;

    #[test]
    fn floating_details_reserves_the_overlay_and_a_docked_panel_does_not() {
        let wide = super::INSPECTOR_OVERLAY_SPAN + super::MIN_CHAT_BESIDE_DETAILS + 80.;
        assert_eq!(super::chat_right_reserve(false, false, wide), 0.);
        assert_eq!(super::chat_right_reserve(false, true, wide), 0.);
        assert_eq!(super::chat_right_reserve(true, true, wide), 0.);
        assert_eq!(
            super::chat_right_reserve(true, false, wide),
            super::INSPECTOR_OVERLAY_SPAN
        );
        assert_eq!(
            super::chat_right_reserve(true, false, super::MIN_CHAT_BESIDE_DETAILS),
            0.,
            "a pane that is already the minimum keeps its width"
        );
        let tight = super::MIN_CHAT_BESIDE_DETAILS + 100.;
        assert_eq!(super::chat_right_reserve(true, false, tight), 100.);
    }

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

    #[test]
    fn the_subagent_window_stacks_left_of_details_drawer_and_diff() {
        let wide = 1520.;
        // Alone: clear of Details by the panel gap.
        let (right, width) =
            super::subagent_window_bounds(wide, super::INSPECTOR_OVERLAY_SPAN, false, false);
        assert_eq!(right, super::INSPECTOR_OVERLAY_SPAN + 12.);
        assert_eq!(width, super::SUBAGENT_WINDOW_WIDTH);
        // With the drawer open it moves left of the drawer.
        let (right, _) =
            super::subagent_window_bounds(wide, super::INSPECTOR_OVERLAY_SPAN, true, false);
        assert!(right > super::INSPECTOR_OVERLAY_SPAN + 480.);
        // With drawer and diff open it moves left of both when the window
        // is wide enough to stack all three.
        let (right, _) =
            super::subagent_window_bounds(1920., super::INSPECTOR_OVERLAY_SPAN, true, true);
        assert!(right > super::INSPECTOR_OVERLAY_SPAN + 480. + 560.);
        // When everything cannot fit, it stays inside the viewport.
        let (right, _) =
            super::subagent_window_bounds(700., super::INSPECTOR_OVERLAY_SPAN, true, true);
        assert!(right + super::SUBAGENT_WINDOW_WIDTH <= 700.);
    }

    #[test]
    fn context_bar_fraction_is_zero_when_empty_and_proportional_to_window() {
        assert_eq!(super::context_bar_fraction(0, 0), 0.0);
        assert_eq!(super::context_bar_fraction(0, 200_000), 0.0);
        assert!((super::context_bar_fraction(50_000, 200_000) - 0.25).abs() < f32::EPSILON);
        assert!((super::context_bar_fraction(200_000, 200_000) - 1.0).abs() < f32::EPSILON);
        assert!((super::context_bar_fraction(250_000, 200_000) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn responsive_panel_bounds_keep_drawer_and_diff_on_screen_across_window_widths() {
        for width in [960., 1100., 1280., 1360., 1520.] {
            for span in [
                0.,
                super::INSPECTOR_DOCKED_WIDTH,
                super::INSPECTOR_OVERLAY_SPAN,
            ] {
                for drawer_open in [false, true] {
                    let ((drawer_right, drawer_w), (diff_right, diff_w)) =
                        super::responsive_panel_bounds(width, span, drawer_open);
                    let drawer_left = width - drawer_right - drawer_w;
                    let diff_left = width - diff_right - diff_w;
                    assert!(
                        drawer_left >= 12. - f32::EPSILON,
                        "drawer left {drawer_left} off-screen at width {width}, span {span}"
                    );
                    assert!(
                        diff_left >= 12. - f32::EPSILON,
                        "diff left {diff_left} off-screen at width {width}, span {span}, drawer {drawer_open}"
                    );
                    assert!(drawer_w >= 280.);
                    assert!(diff_w >= 280.);
                }
            }
        }
    }
}
