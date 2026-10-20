//! The composer bar: a floating rounded card at the bottom of the conversation column.
//!
//! Model, reasoning, and folder sit as pill chips inside the bar, alongside a
//! `⌘K` / `Ctrl+K` hint; typing `@` references a project file. While a turn
//! is running the round action is Stop: it ends the whole turn, including
//! every subagent, and holds the queue. A draft sent with Enter queues for
//! after the turn; the arrow beside Stop queues too, and Ctrl+Enter
//! interrupts the turn and sends the draft right away. Ctrl+G steers: the
//! draft reaches the running turn before its next step without stopping it,
//! and waits in a strip above the queue until the agent reads it. Each
//! running scout or artisan is cancelled from its own `agent` row in the
//! transcript.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    BoxShadow, Context, Focusable as _, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, hsla, px,
};

use crate::i18n::t;
use crate::ui::command_palette::shortcut_label;
use crate::ui::{desk::Desk, ellipsis, project_label, skin};
use crate::view_model::WorkspaceState;
use crate::view_model::{selected_model_supports_reasoning, selected_reasoning_level};
use crate::workspace::Workspace;

/// Resting composer hint. The product name does not belong in the field.
const COMPOSER_PLACEHOLDER: (&str, &str) = ("Ask the agent", "向代理提问");
const QUEUE_PLACEHOLDER: (&str, &str) = (
    "Queue; Ctrl+Enter interrupts; Ctrl+G steers",
    "回车排队 · Ctrl+Enter 打断发送 · Ctrl+G 引导",
);

/// Placeholder for the composer. While a turn is running, typed text queues
/// for after the turn; Ctrl+Enter interrupts and sends it right away, and
/// Ctrl+G hands it to the running turn.
#[must_use]
pub(crate) fn composer_placeholder(sending: bool) -> &'static str {
    let (english, chinese) = if sending {
        QUEUE_PLACEHOLDER
    } else {
        COMPOSER_PLACEHOLDER
    };
    t(english, chinese)
}

/// Centered composer measure. Narrower than the chat pane; prose and tool
/// cards stay full width and left-aligned.
pub(super) const COMPOSER_MAX_PX: f32 = 760.;

/// Bottom composer: a floating rounded card with soft elevation, pill chips for
/// folder, model, and thinking, and a circular send/stop action. The field soft-wraps inside a fixed three-row box and
/// scrolls once the draft is longer.
pub(super) fn render_composer(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let composer = workspace.composer().clone();
    if let Some((text, focus)) = workspace.take_composer_set() {
        composer.update(cx, |state, cx| state.set_value(text, window, cx));
        if focus {
            composer.read(cx).focus_handle(cx).focus(window, cx);
        }
    }
    let model_label: SharedString = model_button_label(workspace.vm()).into();
    let thinking_label: SharedString = thinking_button_label(workspace.vm()).into();
    // The chip truncates a long model name; the tooltip keeps the provider
    // prefix and the full name visible.
    let model_tooltip: Option<SharedString> =
        workspace.vm().selected_provider.as_deref().map(|provider| {
            format!(
                "{} · {}",
                crate::ui::model_picker::provider_display_name(workspace.vm(), provider),
                model_label
            )
            .into()
        });
    let show_thinking = selected_model_supports_reasoning(workspace.vm());
    let has_session = workspace.vm().active.is_some();
    // A manual compact is not a turn, but it locks the composer the same
    // way: Enter queues, Ctrl+Enter cancels it and sends the draft.
    let sending = workspace.vm().sending || workspace.vm().compacting;
    let has_draft = !workspace.vm().composer_draft.trim().is_empty();
    let has_queue = !workspace.vm().queued.is_empty();
    let can_send = if has_session {
        has_draft || has_queue
    } else {
        // A draft alone enables the arrow: `submit_from_welcome` answers a
        // missing folder with a toast instead of leaving a dead button.
        has_draft
    };
    let steer_panel = (!workspace.vm().pending_steers.is_empty())
        .then(|| render_pending_steers(&workspace.vm().pending_steers, cx).into_any_element());
    let queue_panel = if has_queue {
        let paused = workspace.vm().queue_paused;
        Some(
            render_queued_followups(&workspace.vm().queued, sending, paused, cx).into_any_element(),
        )
    } else {
        None
    };
    let compacting = workspace.vm().compacting;
    let has_pending_ask = workspace.vm().pending_ask.is_some();
    if workspace.composer_busy != sending {
        workspace.composer_busy = sending;
        let placeholder = composer_placeholder(sending);
        composer.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx);
        });
    }
    let thinking_chip = show_thinking.then(|| {
        let menu = workspace
            .vm()
            .reasoning_menu_open
            .then(|| super::menus::render_thinking_menu(workspace, window, cx));
        div()
            .relative()
            .child(composer_pill_chip(
                ComposerChip {
                    id: "thinking",
                    icon: None,
                    label: thinking_label.clone(),
                    tooltip: None,
                    open: workspace.vm().reasoning_menu_open,
                    max_w: px(116.),
                },
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
            .child(composer_pill_chip(
                ComposerChip {
                    id: "model",
                    icon: None,
                    label: model_label,
                    tooltip: model_tooltip,
                    open: workspace.vm().model_menu_open,
                    max_w: px(176.),
                },
                |workspace, window, cx| {
                    let open = !workspace.vm().model_menu_open;
                    workspace.on_toggle_model_menu(open, window, cx);
                },
                cx,
            ))
            .children(menu)
            .into_any_element()
    };
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
    // The chip shows the folder's basename; the tooltip carries the full path.
    let project_tooltip: Option<SharedString> = session_project.as_deref().map(SharedString::from);
    // The ask card docks right above the input like Devin's: a frosted
    // panel, one question at a time. It is built here — before `theme`
    // borrows `cx` — so the builder chain below stays borrow-free.
    let ask_card = if has_pending_ask {
        Some(super::ask::render_ask_card(workspace, window, cx))
    } else {
        None
    };
    let theme = cx.theme();
    // The manual-compact wait lives where the queue and ask cards do: a
    // frosted row above the composer, gone when `CompactFinished` lands.
    let compact_card = compacting.then(|| {
        skin::popover_panel("compact-card", theme)
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .child(crate::ui::lamp(crate::ui::desk::Desk::of(theme).violet))
            .child(
                div()
                    .text_sm()
                    .child(t("Compacting context…", "正在压缩上下文…")),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t("Ctrl+Enter cancels", "Ctrl+Enter 取消")),
            )
            .into_any_element()
    });

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
                .gap_1()
                .when_some(steer_panel, |this, steers| this.child(steers))
                .when_some(queue_panel, |this, queue| this.child(queue))
                .when_some(compact_card, |this, card| this.child(card))
                .when_some(ask_card, |this, card| this.child(card))
                .child(
                    div()
                        .id("composer-card")
                        .flex()
                        .flex_col()
                        .justify_between()
                        .gap(skin::space(2))
                        .px(skin::space(3))
                        .py(skin::space(2))
                        .w_full()
                        .min_h(px(56.))
                        .rounded(px(20.))
                        .border_1()
                        .border_color(skin::glass_border(theme))
                        .bg(skin::popover(theme))
                        .shadow(composer_elevation(theme.is_dark()))
                        .child(
                            div()
                                .id("composer-input")
                                .flex()
                                .flex_col()
                                .w_full()
                                .min_w_0()
                                .text_sm()
                                .child(
                                    // The state pins the field at three
                                    // rows (auto_grow 3..3): long drafts
                                    // scroll inside instead of growing
                                    // the composer card.
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
                                .gap(px(6.))
                                .w_full()
                                .min_w_0()
                                .child(composer_pill_chip(
                                    ComposerChip {
                                        id: "project",
                                        icon: Some(IconName::Folder),
                                        label: project_chip_label,
                                        tooltip: project_tooltip,
                                        open: false,
                                        max_w: px(148.),
                                    },
                                    |workspace, _window, cx| {
                                        workspace.on_open_chat_folder_picker(cx);
                                    },
                                    cx,
                                ))
                                .child(model_chip)
                                .when_some(thinking_chip, |this, chip| this.child(chip))
                                .child(div().flex_1().min_w(px(8.)))
                                .when_some(context_meter(workspace), |this, meter| {
                                    this.child(composer_meter_chip(meter, theme))
                                })
                                .child(composer_palette_hint(cx))
                                // While a turn runs the arrow stays: it
                                // queues the draft; the stop sits beside it.
                                .when(sending && has_draft, |this| {
                                    this.child(composer_send_button(cx))
                                })
                                .child(composer_round_button(sending, can_send, cx)),
                        ),
                ),
        )
}

fn composer_elevation(dark: bool) -> Vec<BoxShadow> {
    let alpha = if dark { 0.35 } else { 0.08 };
    vec![BoxShadow::new(px(0.), px(8.), hsla(0., 0., 0., alpha)).blur_radius(px(24.))]
}

/// Send arrow while idle, stop square while a turn runs.
///
/// Stop ends the whole turn and every subagent. It sits where the send
/// arrow sits.
fn composer_round_button(
    sending: bool,
    can_send: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let active = sending || can_send;
    div()
        .id(if sending {
            "composer-stop"
        } else {
            "composer-send"
        })
        .size(px(30.))
        .flex_shrink_0()
        .ml_auto()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(if active {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .when(active, |this| {
            this.cursor_pointer()
                .hover(|style| style.bg(theme.secondary_hover))
        })
        .on_click(cx.listener(move |workspace, _, window, cx| {
            if sending {
                workspace.on_cancel_chat(cx);
            } else if can_send {
                workspace.on_send(window, cx);
            }
        }))
        // Two round icons sit side by side while a turn runs; the tooltip
        // tells the queueing arrow from the stop that ends everything.
        .when(sending, |this| {
            this.tooltip(|window, cx| {
                Tooltip::new(t(
                    "Stop this turn and every subagent",
                    "停止本轮和所有子代理",
                ))
                .build(window, cx)
            })
        })
        .when(sending, |this| {
            this.child(div().size(px(9.)).rounded(px(2.)).bg(theme.foreground))
        })
        .when(!sending, |this| {
            this.child(Icon::new(IconName::ArrowUp).small())
        })
}

/// The send arrow shown next to the stop square while a turn runs: click
/// queues the draft for after the turn, same as Enter.
fn composer_send_button(cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("composer-send-running")
        .size(px(30.))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.foreground)
        .cursor_pointer()
        .hover(|style| style.bg(theme.secondary_hover))
        .on_click(cx.listener(move |workspace, _, window, cx| {
            workspace.on_send(window, cx);
        }))
        .tooltip(|window, cx| {
            Tooltip::new(t(
                "Queue for after this turn (Ctrl+Enter interrupts)",
                "排队到本轮之后（Ctrl+Enter 打断发送）",
            ))
            .build(window, cx)
        })
        .child(Icon::new(IconName::ArrowUp).small())
}

/// One pill chip in the composer row (folder, model, thinking). The label
/// truncates at `max_w`; when `tooltip` is set, hovering shows the
/// untruncated text.
struct ComposerChip {
    id: &'static str,
    icon: Option<IconName>,
    label: SharedString,
    tooltip: Option<SharedString>,
    open: bool,
    max_w: gpui_kit::Pixels,
}

fn composer_pill_chip(
    chip: ComposerChip,
    on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let ink = if chip.open {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    div()
        .id(format!("composer-{}-chip", chip.id))
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_1()
        .gap(px(5.))
        .min_w_0()
        .max_w(chip.max_w)
        .px(px(9.))
        .h(px(26.))
        .rounded(px(6.))
        .when(chip.open, |this| this.bg(theme.secondary_hover))
        .text_xs()
        .cursor_pointer()
        .text_color(ink)
        .hover(|this| this.bg(theme.secondary_hover).text_color(theme.foreground))
        .on_click(cx.listener(move |workspace, _, window, cx| {
            on_click(workspace, window, cx);
        }))
        .when_some(chip.tooltip, |this, text| {
            this.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
        })
        .when_some(chip.icon, |this, icon| {
            this.child(Icon::new(icon).xsmall().flex_shrink_0())
        })
        .child(div().min_w_0().truncate().child(chip.label))
        .child(Icon::new(IconName::ChevronDown).xsmall().flex_shrink_0())
}

/// Quiet shortcut badge that opens the command palette (`⌘K` / `Ctrl+K`).
fn composer_palette_hint(cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("composer-palette-hint")
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .px(px(7.))
        .h(px(24.))
        .rounded(px(6.))
        .text_xs()
        .text_color(theme.muted_foreground)
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary_hover).text_color(theme.foreground))
        .on_click(cx.listener(|workspace, _, window, cx| {
            workspace.toggle_command_palette(window, cx);
        }))
        .child(shortcut_label())
}

/// Steers the running turn has not read yet. The agent takes them before
/// its next step; one the turn ends without reading moves to the queue.
fn render_pending_steers(items: &[String], cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = Desk::of(theme);
    skin::popover_panel("composer-steers", theme)
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .px_3()
        .py_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(crate::ui::lamp(desk.violet))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                        .text_color(desk.faint)
                        .child(format!("{}  {}", t("STEERING", "引导中"), items.len())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t(
                            "The agent reads these before its next step.",
                            "代理在下一步之前读取。",
                        )),
                ),
        )
        .children(items.iter().enumerate().map(|(index, text)| {
            let preview: SharedString = ellipsis(text, 72).into();
            div()
                .id(SharedString::from(format!("steer-{index}")))
                .min_w_0()
                .h(px(24.))
                .pl(px(15.))
                .flex()
                .items_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().min_w_0().flex_1().truncate().child(preview))
        }))
}

/// Queued follow-ups with explicit actions: recall (back into the composer
/// to edit or drop), interrupt and send, or send a queue a stop is holding.
/// The strip rides in the same frosted panel as the diff card so it reads
/// as a surface, not loose text floating above the composer.
fn render_queued_followups(
    items: &[String],
    sending: bool,
    paused: bool,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = Desk::of(theme);
    let hint = if paused {
        t(
            "Held after Stop. Send it, or recall to edit.",
            "已停止，暂不发送。可发送，或撤回到输入框修改。",
        )
    } else {
        t("Sends when this turn ends.", "本轮结束后依次发送。")
    };
    let many = items.len() > 1;
    let send_label = if !sending {
        t("Send", "发送")
    } else if paused {
        t("Send after this turn", "本轮后发送")
    } else {
        t("Interrupt & send", "打断并发送")
    };
    skin::popover_panel("composer-queue", theme)
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .px_3()
        .py_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(crate::ui::lamp(if paused {
                    desk.faint
                } else {
                    desk.amber
                }))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                        .text_color(desk.faint)
                        .child(format!("{}  {}", t("QUEUED", "已排队"), items.len())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(hint),
                )
                .when(many, |this| {
                    this.child(
                        queue_action("queued-recall-all", t("Recall all", "全部撤回"), false, cx)
                            .on_click(cx.listener(|workspace, _, window, cx| {
                                workspace.on_recall_queued(None, window, cx);
                            })),
                    )
                })
                .child(
                    queue_action("queued-send", send_label, true, cx).on_click(cx.listener(
                        move |workspace, _, _, cx| {
                            if sending && !paused {
                                workspace.on_interrupt_queued(0, cx);
                            } else {
                                workspace.on_resume_queued(cx);
                            }
                        },
                    )),
                ),
        )
        .children(items.iter().enumerate().map(|(index, text)| {
            let preview: SharedString = ellipsis(text, 72).into();
            div()
                .id(SharedString::from(format!("queued-{index}")))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .min_w_0()
                .h(px(24.))
                .pl(px(15.))
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
                    queue_icon(
                        SharedString::from(format!("queued-recall-{index}")),
                        IconName::Pencil,
                        t("Recall to edit", "撤回到输入框修改"),
                        theme,
                    )
                    .on_click(cx.listener(move |workspace, _, window, cx| {
                        workspace.on_recall_queued(Some(index), window, cx);
                    })),
                )
                .child(
                    queue_icon(
                        SharedString::from(format!("queued-remove-{index}")),
                        IconName::X,
                        t("Remove from the queue", "移出队列"),
                        theme,
                    )
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        workspace.on_remove_queued(index, cx);
                    })),
                )
                .child(
                    queue_icon(
                        SharedString::from(format!("queued-send-{index}")),
                        IconName::CornerDownLeft,
                        if sending {
                            t("Interrupt this turn and send now", "打断本轮并立即发送")
                        } else {
                            t("Send now", "立即发送")
                        },
                        theme,
                    )
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        workspace.on_interrupt_queued(index, cx);
                    })),
                )
        }))
}

/// One 20px icon button on a queued-message row: edit back into the composer,
/// drop the item, or send it now (interrupting a running turn).
fn queue_icon(
    id: impl Into<gpui_kit::ElementId>,
    icon: IconName,
    label: &'static str,
    theme: &gpui_kit::component::theme::Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .rounded(skin::radius_xs())
        .cursor_pointer()
        .hover(|this| {
            this.bg(skin::frost_hover(theme))
                .text_color(theme.foreground)
        })
        .child(Icon::new(icon).xsmall())
}

/// A compact text action in the queue strip. `strong` is the send action.
fn queue_action(
    id: impl Into<gpui_kit::ElementId>,
    label: &'static str,
    strong: bool,
    cx: &Context<Workspace>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(22.))
        .px_2()
        .flex()
        .items_center()
        .rounded(skin::radius_xs())
        .text_xs()
        .text_color(if strong {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .cursor_pointer()
        .hover(|this| {
            this.bg(skin::frost_hover(theme))
                .text_color(theme.foreground)
        })
        .child(label)
}

fn composer_meter_chip(
    meter: ComposerMeter,
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
        .h(px(26.))
        .rounded(px(6.))
        .text_xs()
        // The resting label is the window ratio alone; the cache figures
        // and the turn's in/out ride the hover tooltip instead of crowding
        // the chip row.
        .when(!meter.detail.is_empty(), |this| {
            let detail: SharedString = meter.detail.clone().into();
            this.tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
        })
        .when_some(meter.parts.remaining_percent, |row, percent| {
            row.child(cache_ring(percent, theme))
        })
        .when(!meter.parts.ratio.is_empty(), |row| {
            row.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(meter.parts.ratio.clone()),
            )
        })
}

/// The visible arc: the share of the window in use.
fn used_percent(remaining_percent: u64) -> u64 {
    100 - remaining_percent.min(100)
}

/// A 16px ring whose arc is the share of the model's context window in use.
///
/// The ring fills as the latest prompt grows, so it matches the `used /
/// window` ratio beside it; a free-budget ring read backwards and looked
/// nearly full at 20% used. The cache-hit figure stays on the hover detail.
fn cache_ring(
    remaining_percent: u64,
    theme: &gpui_kit::component::theme::Theme,
) -> impl IntoElement {
    div()
        .id("composer-cache-ring")
        .flex_shrink_0()
        .size(skin::space(4))
        .child(
            // The kit sizes the ring in rems; fill the fixed slot so the L
            // and XL interface scales do not push it into the label.
            ProgressCircle::new("composer-cache-ring-progress")
                .value(used_percent(remaining_percent) as f32)
                .color(theme.primary)
                .size_full(),
        )
}

/// The chip keeps the prompt/window ratio on screen; `detail` carries the
/// cache share, the cache-read count, and the turn's in/out for the hover
/// tooltip.
struct ComposerMeter {
    parts: crate::view_model::ContextMeterParts,
    detail: String,
}

/// Latest prompt size and cache read, shown on the chip row itself.
///
/// The inspector has the same figures, but it stays closed until the title
/// bar opens it. The chip reads `used / window` at rest; hovering fills in
/// the cache and token detail.
fn context_meter(workspace: &Workspace) -> Option<ComposerMeter> {
    let vm = workspace.vm();
    let used = vm.context_used;
    let cached = vm.context_cache;
    let window = crate::view_model::model_context_window(vm);
    if used == 0 && cached == 0 && window == 0 {
        return None;
    }
    // With no usage yet the chip still shows `0 / window`, so the budget is
    // visible from the first message instead of appearing after the first
    // reply.
    // The hit and cache figures are the latest prompt's; the in/out sums are
    // the whole turn's, so each carries its own scope label.
    let parts = crate::view_model::context_meter_parts(
        used,
        window,
        cached,
        t("cached", "缓存"),
        t("last request hit", "最近请求命中"),
    );
    let selected_model = vm.selected_model.clone();
    let model_of = |turn: &crate::view_model::TurnStats| {
        selected_model
            .as_deref()
            .is_none_or(|selected| turn.model == selected)
    };
    let turn = vm
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
    let mut detail: Vec<String> = Vec::new();
    if !parts.hit.is_empty() {
        detail.push(parts.hit.clone());
    }
    if !parts.cache.is_empty() {
        detail.push(parts.cache.clone());
    }
    if let Some((label, turn)) = turn {
        detail.push(format!(
            "{label} {} {} · {} {}",
            crate::ui::compact_count(turn.input),
            t("in", "入"),
            crate::ui::compact_count(turn.output),
            t("out", "出")
        ));
    }
    let meter = ComposerMeter {
        detail: detail.join(" · "),
        parts,
    };
    (!meter.parts.ratio.is_empty() || !meter.parts.cache.is_empty()).then_some(meter)
}

fn model_button_label(vm: &WorkspaceState) -> String {
    crate::ui::model_picker::selected_model_label(vm)
}

/// Chip label for the effective thinking level.
///
/// Labels stay English and short: the chip is at most 116 px wide, and
/// longer spellings such as "Thinking · Default" were cut to "Thinking · D…".
/// The menu rows keep the longer descriptions.
fn thinking_button_label(vm: &WorkspaceState) -> String {
    thinking_chip_label(&selected_reasoning_level(vm)).to_owned()
}

fn thinking_chip_label(level: &str) -> &str {
    match level {
        "default" => "Thinking",
        "off" => "Thinking off",
        "on" => "Thinking on",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        "max" => "Max",
        other => other,
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "test-support"))]
mod wrap_layout;
