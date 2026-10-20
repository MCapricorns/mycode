//! Right panel: live subagent progress, the working tree, and model usage.
//! The full changes drawer also lives here — the inspector list is a preview,
//! the drawer is where a large working tree is browsable.
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px, relative,
};

use crate::i18n::t;
use crate::workspace::Workspace;

mod changes;
mod subagents;

use self::changes::render_changes;
pub(super) use self::changes::{render_changes_drawer, render_diff_panel};
pub(super) use self::subagents::render_subagent_window;
use self::subagents::render_subagents;

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
/// The review pair reads as one split view: a file column the width of the
/// Details overlay, and a diff wide enough for code beside it.
const CHANGES_DRAWER_WIDTH: f32 = 320.;
const DIFF_PANEL_WIDTH: f32 = 640.;
const PANEL_GAP: f32 = 12.;
const PANEL_MIN_WIDTH: f32 = 280.;

// The diff carries code, the drawer only paths.
const _: () = assert!(DIFF_PANEL_WIDTH > CHANGES_DRAWER_WIDTH);

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

/// The shared frame of the changes drawer and the diff panel: the same top,
/// bottom, corners, fill, and edge, so side by side they read as one split
/// view instead of a short card next to a tall one.
pub(crate) fn review_panel(
    id: &'static str,
    right: f32,
    width: f32,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .occlude()
        .absolute()
        .top(px(super::title_bar::BAR_HEIGHT + 8.))
        .bottom(px(PANEL_GAP))
        .right(px(right))
        .w(px(width))
        .flex()
        .flex_col()
        .rounded(crate::ui::skin::radius_card())
        .border_1()
        .border_color(crate::ui::skin::border_strong(theme))
        .bg(crate::ui::skin::popover(theme))
        .shadow(crate::ui::skin::elevation_shadow(theme))
        .overflow_hidden()
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .on_click(|_, _, cx| {
            cx.stop_propagation();
        })
}

const SUBAGENT_WINDOW_WIDTH: f32 = 320.;
/// Bottom inset for the subagent window. It stops above the tallest
/// composer (input cap, toolbar, jump row) so its occluding body never
/// covers the send or stop button where the two overlap horizontally.
const SUBAGENT_WINDOW_BOTTOM_CLEAR: f32 = 248.;

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

/// Inspector as a right-hand drawer. The chat column reserves the drawer's
/// width on its right so the transcript and composer stop at its left edge.
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
        .font_weight(crate::ui::skin::WEIGHT_STRONG)
        .text_color(theme.muted_foreground)
        .child(text.to_uppercase())
}

fn render_model_usage(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let vm = workspace.vm();
    let selected_model = vm
        .selected_model
        .clone()
        .or_else(|| vm.last_turn.as_ref().map(|turn| turn.model.clone()))
        .or_else(|| vm.live_turn.as_ref().map(|turn| turn.model.clone()));
    // Display names, like the composer chip: the catalog name, then the id.
    let provider_id = vm.selected_provider.as_deref().unwrap_or_default();
    let model = selected_model.as_deref().map(|model| {
        if provider_id.is_empty() {
            model.to_owned()
        } else {
            crate::view_model::model_display_name(vm, provider_id, model)
        }
    });
    let provider = if provider_id.is_empty() {
        String::new()
    } else {
        crate::view_model::provider_display_name(vm, provider_id)
    };
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
    // while the context meter is the latest prompt alone; the labels and
    // the request count say so.
    // The hit row shares the sum row's timeframe, so it reads "this turn"
    // only while the turn still runs and "last turn" once the sums are the
    // last turn's.
    let turn_row = vm
        .live_turn
        .as_ref()
        .filter(|turn| model_of(turn))
        .map(|turn| {
            (
                t("This turn, total", "本轮合计"),
                t("Turn cache hit", "本轮缓存命中"),
                turn,
            )
        })
        .or_else(|| {
            vm.last_turn
                .as_ref()
                .filter(|turn| model_of(turn))
                .map(|turn| {
                    (
                        t("Last turn, total", "上一轮合计"),
                        t("Last turn cache hit", "上一轮缓存命中"),
                        turn,
                    )
                })
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
            // The meter is one prompt, so the hit word names that scope:
            // the turn row below it reports the summed hit instead.
            let parts = crate::view_model::context_meter_parts(
                used,
                context_window,
                cached,
                t("cached", "缓存"),
                t("last request hit", "最近请求命中"),
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
        .when_some(turn_row, |this, (label, hit_label, turn)| {
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
                    hit_label,
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

pub(crate) use crate::view_model::model_context_window;

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

#[cfg(test)]
mod tests;
