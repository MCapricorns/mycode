//! The composer bar: a rounded prompt at the bottom of the conversation column.
//!
//! Model and reasoning sit as small controls inside the bar. While a turn is
//! running the arrow slot is Stop: it ends the whole turn, including every
//! subagent. Text sent during that turn is a steer to the main model and does
//! not cancel the children.
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

/// Bottom composer: a rounded card inset from the edges, with the model
/// and thinking controls as quiet text buttons.
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
    if workspace.composer_steer != sending {
        workspace.composer_steer = sending;
        let placeholder = composer_placeholder(sending);
        composer.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx);
        });
    }
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
        .flex_col()
        .items_center()
        .w_full()
        .px_4()
        .pt_2()
        .pb_4()
        .when_some(queue_panel, |this, queue| {
            this.child(div().w_full().max_w(super::COLUMN_MAX).child(queue))
        })
        .child(
            div()
                .id("composer-card")
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .pt_2()
                .pb_2()
                .w_full()
                .max_w(super::COLUMN_MAX)
                .rounded(px(16.))
                .border_1()
                .border_color(skin::glass_border(theme))
                .bg(skin::popover(theme))
                .child(
                    div()
                        .id("composer-input")
                        .flex()
                        .flex_row()
                        .items_center()
                        .w_full()
                        .min_w_0()
                        .min_h(px(36.))
                        .text_sm()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(Textarea::new(&composer).appearance(false).bordered(false)),
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
                                .size(px(28.))
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
                        .child(composer_text_button(
                            "model",
                            model_label,
                            workspace.vm().model_menu_open,
                            px(168.),
                            |workspace, window, cx| {
                                let open = !workspace.vm().model_menu_open;
                                workspace.on_toggle_model_menu(open, window, cx);
                            },
                            cx,
                        ))
                        .when(show_thinking, |this| {
                            this.child(composer_text_button(
                                "thinking",
                                thinking_label,
                                workspace.vm().reasoning_menu_open,
                                px(96.),
                                |workspace, _window, cx| {
                                    let open = !workspace.vm().reasoning_menu_open;
                                    workspace.on_toggle_reasoning_menu(open, cx);
                                },
                                cx,
                            ))
                        })
                        .when_some(context_meter(workspace), |this, meter| {
                            this.child(
                                div()
                                    .id("composer-context-meter")
                                    .flex()
                                    .flex_shrink_1()
                                    .items_center()
                                    .gap_1()
                                    .min_w_0()
                                    .max_w(px(280.))
                                    .px_2()
                                    .h(px(28.))
                                    .text_xs()
                                    .when(!meter.ratio.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(theme.muted_foreground)
                                                .child(meter.ratio.clone()),
                                        )
                                    })
                                    .when(!meter.hit.is_empty(), |row| {
                                        let hit = if meter.ratio.is_empty() {
                                            meter.hit.clone()
                                        } else {
                                            format!("· {}", meter.hit)
                                        };
                                        row.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_color(theme.foreground)
                                                .child(hit),
                                        )
                                    })
                                    .when(!meter.cache.is_empty(), |row| {
                                        let cache =
                                            if meter.ratio.is_empty() && meter.hit.is_empty() {
                                                meter.cache.clone()
                                            } else {
                                                format!("· {}", meter.cache)
                                            };
                                        row.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_color(theme.foreground)
                                                .child(cache),
                                        )
                                    }),
                            )
                        })
                        .child(composer_round_button(sending, can_send, cx)),
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
        .size(px(30.))
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

/// Quiet text button in the composer footer.
fn composer_text_button(
    id: &str,
    label: SharedString,
    open: bool,
    max_w: gpui_kit::Pixels,
    on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(format!("composer-{id}-chip"))
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_1()
        .min_w(px(72.))
        .max_w(max_w)
        .px_2()
        .h(px(28.))
        .rounded(px(10.))
        .text_xs()
        .cursor_pointer()
        .text_color(if open {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .when(open, |this| this.bg(theme.accent.opacity(0.65)))
        .hover(|this| this.bg(theme.secondary_hover).text_color(theme.foreground))
        .on_click(cx.listener(move |workspace, _, window, cx| {
            on_click(workspace, window, cx);
        }))
        .child(div().flex_1().min_w_0().truncate().child(label))
}

/// Follow-ups waiting behind the in-flight turn; each row can be dismissed.
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
