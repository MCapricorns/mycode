//! The composer bar: a floating rounded card at the bottom of the conversation column.
//!
//! Model, reasoning, and folder sit as pill chips inside the bar, alongside a
//! `⌘K` / `Ctrl+K` hint; typing `@` references a project file. While a turn
//! is running the round action is Stop: it ends the whole turn, every
//! subagent, and any tool still running. Enter on a busy turn no longer
//! sends at once: a small choice floats above the composer — steer the
//! running turn (it reads the draft before its next step, without stopping
//! anything) or interrupt and send (the turn stops, the draft goes out
//! right after). Enter confirms the steer, Ctrl+Enter the interrupt, and
//! Escape closes the choice with the draft untouched. Ctrl+G and the arrow
//! beside Stop steer directly, without the choice. Steers wait in a strip
//! above the composer until the agent reads them; each one carries an Undo
//! that takes it back into the composer while it is still unread. Each
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
const STEER_PLACEHOLDER: (&str, &str) = (
    "Enter chooses: steer or interrupt and send",
    "回车选择：引导或打断并发送",
);

/// Placeholder for the composer. While a turn is running, Enter opens the
/// steer-or-interrupt choice above the composer; Ctrl+G steers directly
/// and Ctrl+Enter interrupts without asking.
#[must_use]
pub(crate) fn composer_placeholder(sending: bool) -> &'static str {
    let (english, chinese) = if sending {
        STEER_PLACEHOLDER
    } else {
        COMPOSER_PLACEHOLDER
    };
    t(english, chinese)
}

/// Whether a plain Enter on the composer opens the steer-or-interrupt
/// choice instead of submitting at once: only on a busy turn, only with a
/// text draft, and never with attachments (they cannot steer, so their
/// submit keeps the old path and its waiting toast).
#[must_use]
pub(crate) fn busy_enter_opens_choice(sending: bool, draft: &str, has_attachments: bool) -> bool {
    sending && !draft.trim().is_empty() && !has_attachments
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
    let paste_target = cx.entity();
    if let Some((text, focus)) = workspace.take_composer_set() {
        composer.update(cx, |state, cx| state.set_value(text, window, cx));
        if focus {
            composer.read(cx).focus_handle(cx).focus(window, cx);
        }
    }
    let model_label: SharedString = model_button_label(workspace.vm()).into();
    let thinking_label: SharedString = thinking_button_label(workspace.vm()).into();
    // A long `Provider · Model` keeps its model end: the chip clips from the
    // left, so "…DeepSeek V4.1 Flash" stays readable, and the picker lists
    // the full name. No hover tooltip here: it covered the thinking chip
    // beside it without saying more than the picker already does.
    let show_thinking = selected_model_supports_reasoning(workspace.vm());
    // A manual compact is not a turn, but it locks the composer the same
    // way: Enter waits, Ctrl+Enter stops it and sends the draft.
    let sending = workspace.vm().sending || workspace.vm().compacting;
    let has_draft = !workspace.vm().composer_draft.trim().is_empty();
    let has_attachments = !workspace.attachments.is_empty();
    // A draft or a dropped attachment alone enables the arrow:
    // `submit_from_welcome` answers a missing folder with a toast instead of
    // leaving a dead button.
    let can_send = has_draft || has_attachments;
    let steer_panel = (!workspace.vm().pending_steers.is_empty())
        .then(|| render_pending_steers(&workspace.vm().pending_steers, cx).into_any_element());
    // The steer-or-interrupt choice for the draft still sitting in the
    // field. It paints only while the turn runs; a turn that ends with it
    // open leaves a stale flag the next busy Enter reuses harmlessly,
    // because both picks re-read the live draft and state.
    let send_choice = (workspace.send_choice_open && sending)
        .then(|| render_send_choice(cx).into_any_element());
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
                    clip_start: false,
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
                    tooltip: None,
                    clip_start: true,
                    open: workspace.vm().model_menu_open,
                    max_w: px(240.),
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
                .when_some(compact_card, |this, card| this.child(card))
                .when_some(ask_card, |this, card| this.child(card))
                .when_some(send_choice, |this, choice| this.child(choice))
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
                        // Dropping a picture or a code file attaches it to
                        // the next prompt; folders still bind the project.
                        .can_drop(|value, _, _| value.is::<gpui_kit::ExternalPaths>())
                        .on_drop(cx.listener(
                            |workspace, paths: &gpui_kit::ExternalPaths, _, cx| {
                                workspace.on_drop_attachments(paths.paths(), cx);
                            },
                        ))
                        .when(!workspace.attachments.is_empty(), |this| {
                            this.child(super::render_attachment_chips(workspace, cx))
                        })
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
                                        .w_full()
                                        // A clipboard picture becomes an
                                        // attachment; other pastes fall
                                        // through to the text insertion.
                                        .on_paste(move |item, _, cx| {
                                            paste_target.update(cx, |workspace, cx| {
                                                workspace.on_composer_paste(item, cx)
                                            })
                                        }),
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
                                        clip_start: false,
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
                                // steers the draft in; the stop sits beside
                                // it.
                                .when(sending && has_draft, |this| {
                                    this.child(composer_steer_button(cx))
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
    // A sendable draft gets the primary treatment — the same soft accent
    // fill and thin accent edge as a glass button — so the one action the
    // composer exists for carries the accent instead of sitting grey.
    let send_fill = crate::ui::desk::primary_fill(theme.accent, theme.primary);
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
        .when(!sending && can_send, |this| {
            this.bg(send_fill)
                .border_1()
                .border_color(crate::ui::desk::primary_edge(theme.primary))
                .cursor_pointer()
                .hover(|style| style.bg(crate::ui::desk::deepen(send_fill)))
        })
        .when(sending, |this| {
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

/// The steer arrow shown next to the stop square while a turn runs: click
/// hands the draft to the running turn, same as Enter and Ctrl+G.
fn composer_steer_button(cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("composer-steer-running")
        .size(px(30.))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        // The accent tint tells the steering arrow from the neutral stop
        // beside it before the tooltip can.
        .bg(skin::frost_accent(theme))
        .text_color(theme.primary)
        .cursor_pointer()
        .hover(|style| style.bg(crate::ui::desk::deepen(skin::frost_accent(theme))))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_steer_send(cx);
        }))
        .tooltip(|window, cx| {
            Tooltip::new(t(
                "Steer this turn without stopping it (Ctrl+G)",
                "引导本轮任务，不打断（Ctrl+G）",
            ))
            .build(window, cx)
        })
        .child(Icon::new(IconName::ArrowUp).small())
}

/// One pill chip in the composer row (folder, model, thinking). The label
/// truncates at `max_w`; when `tooltip` is set, hovering shows the
/// untruncated text. `clip_start` drops the label's head instead of its
/// tail, so a `Provider · Model` keeps the model visible.
struct ComposerChip {
    id: &'static str,
    icon: Option<IconName>,
    label: SharedString,
    tooltip: Option<SharedString>,
    clip_start: bool,
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
    let label = if chip.clip_start {
        div()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis_start()
            .child(chip.label)
    } else {
        div().min_w_0().truncate().child(chip.label)
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
        .child(label)
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

/// The choice Enter opens on a busy turn: steer the draft in, or stop the
/// turn and send it. Two rows in the same frosted panel family as the
/// steer strip and the ask card; the key hints name the keys that confirm
/// each row while the choice is open.
fn render_send_choice(cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    skin::popover_panel("composer-send-choice", theme)
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .p_2()
        .child(send_choice_row(
            "composer-send-choice-steer",
            IconName::ArrowUp,
            (skin::frost_accent(theme), theme.primary),
            t("Steer this turn", "引导本轮"),
            t(
                "The agent reads it before its next step; nothing stops.",
                "代理在下一步之前读取，当前回合不中断。",
            ),
            "Enter",
            cx,
            |workspace, cx| workspace.on_send_choice_steer(cx),
        ))
        .child(send_choice_row(
            "composer-send-choice-interrupt",
            IconName::Zap,
            (theme.danger.opacity(0.12), theme.danger),
            t("Interrupt and send", "打断并发送"),
            t(
                "Stops this turn and every subagent, then sends at once.",
                "停止本轮和所有子代理，立即发送这条。",
            ),
            "Ctrl+Enter",
            cx,
            |workspace, cx| workspace.on_send_choice_interrupt(cx),
        ))
        .child(
            div()
                .px_3()
                .pt_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t("Esc closes and keeps the draft.", "Esc 关闭，草稿保留。")),
        )
}

#[allow(clippy::too_many_arguments)]
fn send_choice_row(
    id: &'static str,
    icon: IconName,
    disc: (gpui_kit::Hsla, gpui_kit::Hsla),
    title: &'static str,
    detail: &'static str,
    keys: &'static str,
    cx: &Context<Workspace>,
    pick: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
) -> impl IntoElement {
    let theme = cx.theme();
    let (disc_fill, disc_ink) = disc;
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .w_full()
        .px_3()
        .py_2()
        .rounded(skin::radius_md())
        .cursor_pointer()
        .hover(|style| style.bg(theme.secondary_hover))
        .on_click(cx.listener(move |workspace, _, _, cx| pick(workspace, cx)))
        .child(
            div()
                .flex_shrink_0()
                .size(px(28.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(disc_fill)
                .text_color(disc_ink)
                .child(Icon::new(icon).small()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(skin::WEIGHT_STRONG)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(detail),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .px(px(7.))
                .h(px(22.))
                .rounded(px(6.))
                .border_1()
                .border_color(skin::glass_border(theme))
                .flex()
                .items_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(keys),
        )
}

/// Steers the running turn has not read yet. The agent takes them before
/// its next step; one the turn ends without reading moves to the queue.
/// While a steer waits here its row carries an Undo: the core drops it
/// from the steer inbox and the text returns to the composer.
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
                .h(px(26.))
                .pl(px(15.))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().min_w_0().flex_1().truncate().child(preview))
                .child(
                    div()
                        .id(SharedString::from(format!("steer-retract-{index}")))
                        .flex_shrink_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .h(px(20.))
                        .px(px(7.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.secondary_hover).text_color(theme.foreground))
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.on_retract_steer(index, cx);
                        }))
                        .child(Icon::new(IconName::Undo2).xsmall())
                        .child(t("Undo", "撤销")),
                )
        }))
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
