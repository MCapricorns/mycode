//! Structured questions, docked above the composer.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::Input;
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, Stateful, StatefulInteractiveElement,
    Styled, Window, div, px,
};

use crate::i18n::t;
use crate::ui::{desk::Desk, lamp, short_id, skin};
use crate::workspace::Workspace;

pub(crate) fn render_ask_panel(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let rows = workspace.vm().pending_ask.clone().unwrap_or_default();
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
                .children(rows.iter().enumerate().map(|(index, row)| {
                    let question = row.question.clone();
                    let choices = row.choices.clone();
                    let optional = row.optional;
                    let multiple = row.multiple;
                    let picked = picks.get(index).cloned().unwrap_or_default();
                    div()
                        .id(format!("ask-row-{index}"))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .child(format!("{}. {}", index + 1, question)),
                        )
                        .when(!choices.is_empty(), |this| {
                            this.child(
                                div()
                                    .id(format!("ask-choices-{index}"))
                                    .flex()
                                    .flex_row()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .children(choices.iter().enumerate().map(
                                        |(choice_index, choice)| {
                                            let answer = choice.clone();
                                            let selected =
                                                mycode_tools::builtin::ask_choice_selected(
                                                    &picked, choice,
                                                );
                                            ask_choice_chip(
                                                format!(
                                                    "ask-{index}-{choice_index}-{}",
                                                    short_id(choice)
                                                ),
                                                choice.clone(),
                                                selected,
                                                theme,
                                            )
                                            .on_click(
                                                cx.listener(move |workspace, _, _, cx| {
                                                    workspace.on_pick_ask_choice(
                                                        index,
                                                        answer.clone(),
                                                        single && !multiple,
                                                        cx,
                                                    );
                                                }),
                                            )
                                        },
                                    )),
                            )
                        })
                        .when(optional, |this| {
                            this.child(div().text_xs().opacity(0.5).child(t("Optional", "可选")))
                        })
                }))
                .child(
                    div()
                        .id("ask-free-row")
                        .flex()
                        .flex_row()
                        .flex_nowrap()
                        .items_center()
                        .gap_2()
                        .w_full()
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
                            skin::glass_button("ask-submit", true, theme)
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_submit_free_ask(cx);
                                }))
                                .child(t("Answer", "回答")),
                        ),
                ),
        )
        .into_any_element()
}

/// One ask-user choice. The label stays on one line; a narrow row wraps
/// whole chips. Selected uses the same soft accent fill as a primary button.
fn ask_choice_chip(
    id: impl Into<gpui_kit::ElementId>,
    label: String,
    selected: bool,
    theme: &Theme,
) -> Stateful<gpui_kit::Div> {
    let fill = if selected {
        crate::ui::desk::primary_fill(theme.accent, theme.primary)
    } else {
        theme.transparent
    };
    let hover = if selected {
        crate::ui::desk::deepen(fill)
    } else {
        theme.secondary_hover
    };
    let border = if selected {
        crate::ui::desk::primary_edge(theme.primary)
    } else {
        theme.border
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .h(px(32.))
        .min_w(px(96.))
        .px(px(12.))
        .rounded(px(11.))
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_color(theme.foreground)
        .text_sm()
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |style| style.bg(hover).border_color(border))
        .child(label)
}
