//! Structured questions, floating as a centered modal over the whole window.
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

pub(crate) fn render_ask_overlay(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let rows = workspace.vm().pending_ask.clone().unwrap_or_default();
    let picks = workspace.vm().ask_answers.clone();
    let single = rows.len() == 1;
    let inputs = (0..rows.len())
        .map(|index| workspace.ask_input(index, window, cx))
        .collect::<Vec<_>>();
    let viewport = window.viewport_size();
    let max_height = px((f32::from(viewport.height) * 0.6).clamp(240., 560.));
    let width = px((f32::from(viewport.width) * 0.4).clamp(420., 620.));
    let theme = cx.theme();
    let desk = Desk::of(theme);
    let muted = theme.muted_foreground;
    div()
        .id("ask-layer")
        // Without occlusion GPUI still hovers and clicks the session list,
        // composer, and title bar under the scrim.
        .occlude()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p_8()
        .child(
            // The question blocks the turn; the scrim swallows stray clicks
            // instead of dismissing, so an answer or Decline is the only way
            // out.
            div()
                .id("ask-backdrop")
                .absolute()
                .size_full()
                .bg(skin::modal_scrim(theme))
                .cursor_default()
                .on_click(cx.listener(|_, _, _, _| {})),
        )
        .child(
            skin::dialog_panel("ask-card", theme)
                .relative()
                .w(width)
                .max_h(max_height)
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_3()
                .p_5()
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
                                .font_weight(crate::ui::skin::WEIGHT_STRONG)
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
                                                cx.listener(move |workspace, _, window, cx| {
                                                    workspace.on_pick_ask_choice(
                                                        index,
                                                        answer.clone(),
                                                        single && !multiple,
                                                        window,
                                                        cx,
                                                    );
                                                }),
                                            )
                                        },
                                    )),
                            )
                        })
                        .when(multiple, |this| {
                            this.child(div().text_xs().text_color(muted).child(t(
                                "Select more than one, then answer",
                                "可多选，选完后点回答",
                            )))
                        })
                        .when(optional, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(t("Optional", "可选")),
                            )
                        })
                        .child(
                            div()
                                .id(format!("ask-free-input-{index}"))
                                .w_full()
                                .min_w_0()
                                .h(px(32.))
                                .text_sm()
                                .child(Input::new(&inputs[index])),
                        )
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
                        .justify_end()
                        .child(
                            skin::glass_button("ask-decline", false, theme)
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_dismiss_ask(cx);
                                }))
                                .child(t("Decline", "拒绝")),
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
/// whole chips. Unselected carries its own muted fill so every option reads
/// as a button on the translucent card; selected uses the primary fill.
fn ask_choice_chip(
    id: impl Into<gpui_kit::ElementId>,
    label: String,
    selected: bool,
    theme: &Theme,
) -> Stateful<gpui_kit::Div> {
    let fill = if selected {
        crate::ui::desk::primary_fill(theme.accent, theme.primary)
    } else {
        theme.secondary
    };
    let hover = if selected {
        crate::ui::desk::deepen(fill)
    } else {
        theme.secondary_hover
    };
    let border = if selected {
        crate::ui::desk::primary_edge(theme.primary)
    } else {
        skin::border_strong(theme)
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
