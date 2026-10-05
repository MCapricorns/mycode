//! Structured questions, docked above the composer.
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{Context, InteractiveElement, IntoElement, ParentElement, Styled, Window, div, px};

use crate::i18n::t;
use crate::ui::{desk::Desk, lamp, short_id, skin};
use crate::workspace::Workspace;

pub(crate) fn render_ask_panel(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let rows: Vec<(String, Vec<String>, bool)> =
        workspace.vm().pending_ask.clone().unwrap_or_default();
    let picks = workspace.vm().ask_answers.clone();
    let single = rows.len() == 1;
    let ask_input = workspace.ask_input(window, cx);
    let theme = cx.theme();
    let desk = Desk::of(theme);
    div()
        .id("ask-inline")
        .w_full()
        .px_4()
        .pt_2()
        .flex()
        .justify_center()
        .child(
            div()
                .id("ask-card")
                .w_full()
                .max_w(super::COLUMN_MAX)
                .flex()
                .flex_col()
                .gap_3()
                .p_3()
                .rounded(px(14.))
                .border_1()
                .border_color(skin::glass_border(theme))
                .bg(skin::popover(theme))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(lamp(desk.violet))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(t("The agent needs your input", "代理需要你的输入")),
                        ),
                )
                .children(
                    rows.iter()
                        .enumerate()
                        .map(|(index, (question, choices, optional))| {
                            let picked = picks.get(index).cloned().unwrap_or_default();
                            div()
                                .id(format!("ask-row-{index}"))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_sm().child(format!(
                                    "{}. {}",
                                    index + 1,
                                    question
                                )))
                                .when(!choices.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .id(format!("ask-choices-{index}"))
                                            .flex()
                                            .flex_row()
                                            .flex_wrap()
                                            .gap_2()
                                            .children(choices.iter().enumerate().map(
                                                |(choice_index, choice)| {
                                                    let answer = choice.clone();
                                                    let selected = picked == *choice;
                                                    Button::new(format!(
                                                        "ask-{index}-{choice_index}-{}",
                                                        short_id(choice)
                                                    ))
                                                    .label(choice.clone())
                                                    .small()
                                                    .when(selected, |this| this.primary())
                                                    .when(!selected, |this| this.outline())
                                                    .on_click(cx.listener(
                                                        move |workspace, _, _, cx| {
                                                            workspace.on_pick_ask_choice(
                                                                index,
                                                                answer.clone(),
                                                                single,
                                                                cx,
                                                            );
                                                        },
                                                    ))
                                                },
                                            )),
                                    )
                                })
                                .when(*optional, |this| {
                                    this.child(
                                        div().text_xs().opacity(0.5).child(t("Optional", "可选")),
                                    )
                                })
                        }),
                )
                .child(
                    div()
                        .id("ask-free-row")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("ask-free-input")
                                .flex_1()
                                .min_w_0()
                                .h(px(32.))
                                .text_sm()
                                .child(Input::new(&ask_input)),
                        )
                        .child(
                            Button::new("ask-submit")
                                .label(t("Answer", "回答"))
                                .small()
                                .primary()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_submit_free_ask(cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}
