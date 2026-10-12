//! The composer bar: a rounded prompt at the bottom of the conversation column.
//!
//! Model and reasoning sit as small controls inside the bar. While a turn is
//! running the arrow slot is Stop: it ends the whole turn, including every
//! subagent. Text sent during that turn is a steer to the main model and does
//! not cancel the children. Running scouts and artisans sit above the card,
//! each with its own cancel.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::i18n::t;
use crate::ui::{desk::Desk, ellipsis, project_label, skin};

/// Resting composer hint. The product name does not belong in the field.
const COMPOSER_PLACEHOLDER: (&str, &str) = ("Ask the agent", "向代理提问");
const STEER_PLACEHOLDER: (&str, &str) = ("Steer without interrupting", "追加引导,不打断当前任务");

/// Placeholder for the composer. While a turn is running, typed text steers
/// the current task instead of starting another one.
#[must_use]
pub(crate) fn composer_placeholder(sending: bool) -> &'static str {
    let (english, chinese) = if sending {
        STEER_PLACEHOLDER
    } else {
        COMPOSER_PLACEHOLDER
    };
    t(english, chinese)
}
use crate::view_model::WorkspaceState;
use crate::view_model::{selected_model_supports_reasoning, selected_reasoning_level};
use crate::workspace::Workspace;

/// Centered composer measure. Narrower than the chat pane; prose and tool
/// cards stay full width and left-aligned.
const COMPOSER_MAX_PX: f32 = 720.;

/// Bottom composer: a short centered card, with the model and thinking
/// controls as quiet text buttons. The field soft-wraps and grows with the draft.
pub(super) fn render_composer(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let composer = workspace.composer().clone();
    if let Some(text) = workspace.take_composer_prefill() {
        composer.update(cx, |state, cx| state.set_value(text, window, cx));
    }
    let model_label: SharedString = model_button_label(workspace.vm()).into();
    let thinking_label: SharedString = thinking_button_label(workspace.vm()).into();
    let show_thinking = selected_model_supports_reasoning(workspace.vm());
    let has_session = workspace.vm().active.is_some();
    let sending = workspace.vm().sending;
    let has_draft = !workspace.vm().composer_draft.trim().is_empty();
    let queued = workspace.vm().queued.clone();
    let has_queue = !queued.is_empty();
    let can_send = if has_session {
        has_draft || has_queue
    } else {
        has_draft && crate::view_model::has_open_folder(workspace.vm())
    };
    let queue_panel = if has_queue {
        Some(render_queued_followups(queued, cx).into_any_element())
    } else {
        None
    };
    let running = workspace.vm().live_jobs.clone();
    let running_panel = if running.is_empty() {
        None
    } else {
        Some(render_running_subagents(running, cx).into_any_element())
    };
    if workspace.composer_steer != sending {
        workspace.composer_steer = sending;
        let placeholder = composer_placeholder(sending);
        composer.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx);
        });
    }
    let thinking_chip = show_thinking.then(|| {
        let menu = workspace
            .vm()
            .reasoning_menu_open
            .then(|| super::menus::render_thinking_menu(workspace, cx));
        div()
            .relative()
            .child(composer_text_button(
                "thinking",
                thinking_label.clone(),
                workspace.vm().reasoning_menu_open,
                px(96.),
                |workspace, _window, cx| {
                    let open = !workspace.vm().reasoning_menu_open;
                    workspace.on_toggle_reasoning_menu(open, cx);
                },
                cx,
            ))
            .children(menu)
            .into_any_element()
    });
    let model_chip = {
        let menu = workspace
            .vm()
            .model_menu_open
            .then(|| super::menus::render_model_menu(workspace, window, cx));
        div()
            .relative()
            .child(composer_text_button(
                "model",
                model_label.clone(),
                workspace.vm().model_menu_open,
                px(168.),
                |workspace, window, cx| {
                    let open = !workspace.vm().model_menu_open;
                    workspace.on_toggle_model_menu(open, window, cx);
                },
                cx,
            ))
            .children(menu)
            .into_any_element()
    };
    let theme = cx.theme();
    let session_project = workspace
        .vm()
        .active
        .as_ref()
        .and_then(|conversation| {
            workspace
                .vm()
                .session_projects
                .iter()
                .find(|(id, _)| *id == conversation.session_id)
                .map(|(_, project)| project.clone())
        })
        .or_else(|| workspace.vm().project_dir.clone());
    let project_chip_label: SharedString = session_project
        .as_deref()
        .map(project_label)
        .unwrap_or_else(|| t("Set folder", "选择目录").to_owned())
        .into();

    div()
        .id("composer")
        .flex()
        .flex_row()
        .justify_center()
        .w_full()
        .px_4()
        .pt(skin::space(1))
        .pb(skin::space(3))
        .child(
            div()
                .id("composer-column")
                .w_full()
                .max_w(px(COMPOSER_MAX_PX))
                .min_w_0()
                .flex()
                .flex_col()
                .when_some(running_panel, |this, running| this.child(running))
                .when_some(queue_panel, |this, queue| this.child(queue))
                .child(
                    div()
                        .id("composer-card")
                        .flex()
                        .flex_col()
                        .gap(skin::space(1))
                        .px(skin::space(3))
                        .py(skin::space(1))
                        .w_full()
                        .rounded(skin::radius_lg())
                        .border_1()
                        .border_color(skin::glass_border(theme))
                        .bg(skin::popover(theme))
                        .child(
                            div()
                                .id("composer-input")
                                .flex()
                                .flex_col()
                                .w_full()
                                .min_w_0()
                                .h_auto()
                                .min_h(px(22.))
                                .text_sm()
                                .child(
                                    Textarea::new(&composer)
                                        .appearance(false)
                                        .bordered(false)
                                        .w_full(),
                                ),
                        )
                        .child(
                            div()
                                .id("composer-chip-row")
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .items_center()
                                .gap_1()
                                .w_full()
                                .min_w_0()
                                .child(
                                    div()
                                        .id("composer-plus")
                                        .size(skin::space(6))
                                        .flex()
                                        .flex_shrink_0()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(10.))
                                        .cursor_pointer()
                                        .text_color(theme.muted_foreground)
                                        .hover(|this| this.bg(theme.secondary_hover))
                                        .on_click(cx.listener(|workspace, _, _, cx| {
                                            workspace.on_open_project_dialog(cx);
                                        }))
                                        .child(Icon::new(IconName::Plus).small()),
                                )
                                .child(composer_text_button(
                                    "project",
                                    project_chip_label,
                                    false,
                                    px(140.),
                                    |workspace, _window, cx| {
                                        workspace.on_open_project_dialog(cx);
                                    },
                                    cx,
                                ))
                                .child(model_chip)
                                .when_some(thinking_chip, |this, chip| this.child(chip))
                                .child(div().flex_1().min_w(px(8.)))
                                .when_some(context_meter(workspace), |this, meter| {
                                    this.child(composer_meter_chip(meter, theme))
                                })
                                .child(composer_round_button(sending, can_send, cx)),
                        ),
                ),
        )
}

/// Arrow while idle, stop square while a turn is running.
///
/// Stop ends the whole turn and every subagent. It sits where the send
/// arrow sits.
fn composer_round_button(
    sending: bool,
    can_send: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let fill = super::super::desk::primary_fill(theme.accent, theme.primary);
    let hover = super::super::desk::deepen(fill);
    let edge = super::super::desk::primary_edge(theme.primary);
    div()
        .id(if sending {
            "composer-stop"
        } else {
            "composer-send"
        })
        .size(skin::space(6))
        .flex_shrink_0()
        .ml_auto()
        .rounded(px(10.))
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .when(sending || can_send, |this| this.cursor_pointer())
        .when(sending, |this| {
            this.border_color(theme.border).text_color(theme.foreground)
        })
        .when(!sending, |this| {
            this.bg(if can_send { fill } else { theme.transparent })
                .border_color(if can_send { edge } else { theme.border })
                .text_color(if can_send {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .when(can_send, move |this| {
                    this.hover(move |style| style.bg(hover))
                })
        })
        .on_click(cx.listener(move |workspace, _, window, cx| {
            if sending {
                workspace.on_cancel_chat(cx);
            } else if can_send {
                workspace.on_send(window, cx);
            }
        }))
        .when(sending, |this| {
            this.child(div().size(px(10.)).rounded(px(2.)).bg(theme.foreground))
        })
        .when(!sending, |this| {
            this.child(Icon::new(IconName::ArrowUp).small())
        })
}

/// Plain footer control: muted italic label and a chevron, with no frame.
fn composer_text_button(
    id: &str,
    label: SharedString,
    open: bool,
    max_w: gpui_kit::Pixels,
    on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let ink = if open {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    div()
        .id(format!("composer-{id}-chip"))
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_1()
        .gap(skin::space(1))
        .min_w_0()
        .max_w(max_w)
        .px(skin::space(1))
        .h(skin::space(6))
        .text_xs()
        .italic()
        .cursor_pointer()
        .text_color(ink)
        .hover(|this| this.text_color(theme.foreground))
        .on_click(cx.listener(move |workspace, _, window, cx| {
            on_click(workspace, window, cx);
        }))
        .child(div().min_w_0().truncate().child(label))
        .child(Icon::new(IconName::ChevronDown).xsmall().flex_shrink_0())
}

/// Follow-ups waiting behind the in-flight turn; each row can be dismissed.
fn render_running_subagents(
    jobs: Vec<crate::view_model::LiveJob>,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = Desk::of(theme);
    let capped = crate::view_model::capped_artisan_count(&jobs);
    let count = jobs.len();
    div()
        .id("running-subagents")
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .pb_1()
        .when(capped > 0, |this| {
            this.child(
                div()
                    .id("running-subagents-cap")
                    .text_xs()
                    .whitespace_normal()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .bg(theme.warning.opacity(0.18))
                    .text_color(theme.warning_foreground)
                    .child(crate::view_model::capped_artisan_status(capped)),
            )
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(crate::ui::lamp(desk.amber))
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(desk.faint)
                        .child(format!("{}  {count}", t("RUNNING", "运行中"))),
                ),
        )
        .child(
            div()
                .id("running-subagents-list")
                .w_full()
                .max_h(px(168.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .children(jobs.into_iter().enumerate().map(|(index, job)| {
                    let call_id = job.call_id.clone();
                    let cancel_id = job.call_id.clone();
                    let title = job.title();
                    let step = job.step_label();
                    let warned = job.capped;
                    div()
                        .id(format!("running-subagent-{index}"))
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .px_1()
                        .py(px(2.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|row| row.bg(skin::frost_hover(theme)))
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
                                .child(crate::ui::lamp(if warned {
                                    theme.warning
                                } else {
                                    desk.amber
                                }))
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
                                        .id(format!("running-subagent-cancel-{index}"))
                                        .flex_shrink_0()
                                        .px_1()
                                        .text_xs()
                                        .cursor_pointer()
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
                })),
        )
}

fn render_queued_followups(items: Vec<String>, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = Desk::of(theme);
    div()
        .id("composer-queue")
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .pb_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(crate::ui::lamp(desk.amber))
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(desk.faint)
                        .child(format!("{}  {}", t("QUEUED", "已排队"), items.len())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t("Sends when this turn ends", "本轮结束后发送")),
                )
                .child(
                    div()
                        .id("queued-interrupt")
                        .flex_shrink_0()
                        .px_2()
                        .py(px(2.))
                        .rounded(skin::radius_control())
                        .border_1()
                        .border_color(skin::glass_border(theme))
                        .text_xs()
                        .text_color(theme.foreground)
                        .cursor_pointer()
                        .hover(|this| this.bg(skin::frost_hover(theme)))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_interrupt_queued(0, cx);
                        }))
                        .child(t("Interrupt & send", "打断并发送")),
                ),
        )
        .children(items.into_iter().enumerate().map(|(index, text)| {
            let preview: SharedString = ellipsis(&text, 72).into();
            div()
                .id(SharedString::from(format!("queued-{index}")))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .min_w_0()
                .h(px(22.))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .truncate()
                        .child(preview),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("queued-remove-{index}")))
                        .cursor_pointer()
                        .flex_shrink_0()
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.on_remove_queued(index, cx);
                        }))
                        .child(Icon::new(IconName::X).xsmall()),
                )
        }))
}

fn composer_meter_chip(
    meter: crate::view_model::ContextMeterParts,
    theme: &gpui_kit::component::theme::Theme,
) -> impl IntoElement {
    div()
        .id("composer-context-meter")
        .flex()
        .flex_shrink_1()
        .items_center()
        .gap(skin::space(1))
        .min_w_0()
        .max_w(px(220.))
        .px(skin::space(2))
        .h(skin::space(6))
        .rounded(skin::radius_sm())
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::frost(theme))
        .text_xs()
        .when_some(meter.hit_percent, |row, percent| {
            row.child(cache_ring(percent, theme))
        })
        .when(!meter.ratio.is_empty(), |row| {
            row.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(meter.ratio.clone()),
            )
        })
        .when(!meter.cache.is_empty(), |row| {
            row.child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child(meter.cache.clone()),
            )
        })
}

/// A 16px ring whose stroke is the cache-hit share of the latest prompt.
fn cache_ring(percent: u64, theme: &gpui_kit::component::theme::Theme) -> impl IntoElement {
    let percent = percent.min(100);
    let radius = 6.0_f32;
    let circumference = std::f32::consts::PI * 2. * radius;
    let dash = circumference * (percent as f32) / 100.;
    let gap = circumference - dash;
    let track = hsla_hex(theme.muted_foreground.opacity(0.35));
    let ink = hsla_hex(theme.primary);
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6" fill="none" stroke="{track}" stroke-width="2"/><circle cx="8" cy="8" r="6" fill="none" stroke="{ink}" stroke-width="2" stroke-linecap="round" stroke-dasharray="{dash:.2} {gap:.2}" transform="rotate(-90 8 8)"/></svg>"#
    );
    div()
        .id("composer-cache-ring")
        .flex_shrink_0()
        .size(skin::space(4))
        .child(
            Icon::default()
                .data(svg.as_bytes())
                .with_size(skin::space(4)),
        )
}

fn hsla_hex(color: gpui_kit::Hsla) -> String {
    let rgb = color.to_rgb();
    format!(
        "#{:02x}{:02x}{:02x}",
        (rgb.r * 255.).round() as u8,
        (rgb.g * 255.).round() as u8,
        (rgb.b * 255.).round() as u8
    )
}

/// Latest prompt size and cache read, shown on the chip row itself.
///
/// The inspector has the same figures, but it stays closed until the title
/// bar opens it. The cache piece does not shrink, so a non-zero hit stays
/// readable when the model chip takes the row.
fn context_meter(workspace: &Workspace) -> Option<crate::view_model::ContextMeterParts> {
    let vm = workspace.vm();
    let used = vm.context_used;
    let cached = vm.context_cache;
    if used == 0 && cached == 0 {
        return None;
    }
    let parts = crate::view_model::context_meter_parts(
        used,
        super::super::context::model_context_window(vm),
        cached,
        t("cached", "缓存"),
        t("cache hit", "缓存命中"),
    );
    (!parts.ratio.is_empty() || !parts.cache.is_empty()).then_some(parts)
}

fn model_button_label(vm: &WorkspaceState) -> String {
    crate::ui::model_picker::selected_model_label(vm)
}

fn thinking_button_label(vm: &WorkspaceState) -> String {
    match selected_reasoning_level(vm) {
        // The chip names its rung: a bare "Thinking" read as some unnamed
        // intensity. Default says the provider decides.
        "default" => t("Thinking · Default", "思考 · 默认").to_owned(),
        "off" => t("Thinking off", "思考关").to_owned(),
        "on" => t("Thinking on", "思考开").to_owned(),
        "minimal" => t("Minimal", "极简").to_owned(),
        "low" => t("Low · brief", "低 · 简短").to_owned(),
        "medium" => t("Medium · balanced", "中 · 均衡").to_owned(),
        "high" => t("High · deep", "高 · 深入").to_owned(),
        "xhigh" => t("Extra high", "超高").to_owned(),
        "max" => t("Max", "最高").to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{COMPOSER_PLACEHOLDER, STEER_PLACEHOLDER};

    #[test]
    fn composer_placeholder_asks_the_agent_without_a_product_name() {
        assert_eq!(COMPOSER_PLACEHOLDER.0, "Ask the agent");
        assert_eq!(COMPOSER_PLACEHOLDER.1, "向代理提问");
        assert!(!COMPOSER_PLACEHOLDER.0.contains("MYCode"));
        assert!(!COMPOSER_PLACEHOLDER.1.contains("MYCode"));
        assert!(!COMPOSER_PLACEHOLDER.0.to_ascii_lowercase().contains("chat"));
        assert!(!COMPOSER_PLACEHOLDER.1.contains("对话"));
        assert_eq!(STEER_PLACEHOLDER.0, "Steer without interrupting");
        assert_eq!(STEER_PLACEHOLDER.1, "追加引导,不打断当前任务");
    }
}

#[cfg(all(test, feature = "test-support"))]
mod wrap_layout {
    use gpui_kit::component::input::{Textarea, TextareaState};
    use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::{
        AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
        Styled, TestAppContext, Window, div, px, size,
    };

    struct WrapProbe {
        state: Entity<TextareaState>,
        width: f32,
    }

    impl Render for WrapProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                div()
                    .id("wrap-box")
                    .w(px(self.width))
                    .min_w_0()
                    .child(
                        div()
                            .id("composer-input")
                            .w_full()
                            .min_w_0()
                            .h_auto()
                            .flex()
                            .flex_col()
                            .child(
                                Textarea::new(&self.state)
                                    .appearance(false)
                                    .bordered(false)
                                    .w_full(),
                            )
                            .test_support(),
                    )
                    .test_support(),
            )
        }
    }

    #[gpui_kit::test]
    fn a_long_draft_wraps_and_grows_inside_the_composer_width(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let width = 360.;
        let held: Rc<RefCell<Option<Entity<TextareaState>>>> = Rc::new(RefCell::new(None));
        let slot = held.clone();
        let handle = cx.open_window(size(px(800.), px(400.)), move |window, cx| {
            let state = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .soft_wrap(true)
                    .auto_grow(1, 8)
            });
            *slot.borrow_mut() = Some(state.clone());
            WrapProbe { state, width }
        });
        let state = held.borrow().clone().expect("textarea");
        cx.update_window(handle.into(), |_, window, cx| {
            let draft = "wrap this sentence ".repeat(12);
            state.update(cx, |state, cx| state.set_value(draft, window, cx));
            window.render_frame(cx);
            window.render_frame(cx);
            let box_bounds = window.find("wrap-box").bounds();
            let input = window.find("composer-input").bounds();
            assert!(
                input.right() <= box_bounds.right() + px(1.)
                    && input.size.width <= box_bounds.size.width + px(1.),
                "the field must stay inside the composer width: input {input:?} box {box_bounds:?}"
            );
            assert!(
                input.size.height > px(28.),
                "a long draft must wrap onto another line instead of scrolling sideways: {input:?}"
            );
        })
        .unwrap();
    }
}
