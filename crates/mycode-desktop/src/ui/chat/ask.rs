//! Structured questions, docked right above the composer like Devin's ask
//! card. One question shows at a time: a single-select pick advances to the
//! next, a multi-select or free text waits for Next, and the last answer
//! submits. The card shares the diff panel's frosted recipe; every control
//! is a ghost — no filled buttons on the translucent card.
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Input;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use crate::i18n::t;
use crate::ui::{desk::Desk, lamp, short_id, skin};
use crate::workspace::Workspace;

/// The card mounted inside the composer column while a turn is waiting on
/// `ask_user`. `Workspace::ask_index` tracks the visible question; earlier
/// answers stay in `ask_answers` and the per-question inputs.
pub(crate) fn render_ask_card(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let rows = workspace.vm().pending_ask.clone().unwrap_or_default();
    let picks = workspace.vm().ask_answers.clone();
    let total = rows.len();
    let index = workspace.ask_index().min(total.saturating_sub(1));
    let last = index + 1 >= total;
    // `ask_input` needs `&mut cx`, so it is fetched before `theme` borrows.
    let input = rows
        .get(index)
        .map(|_| workspace.ask_input(index, window, cx));
    let theme = cx.theme();
    let desk = Desk::of(theme);
    let muted = theme.muted_foreground;
    let card = skin::popover_panel("ask-card", theme)
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .px_4()
        .py_3()
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
                )
                .child(div().flex_1())
                .when(total > 1, |this| {
                    this.child(div().text_xs().text_color(muted).child(format!(
                        "{} / {}",
                        index + 1,
                        total
                    )))
                }),
        );
    let (Some(row), Some(input)) = (rows.get(index).cloned(), input) else {
        return card.into_any_element();
    };
    let picked = picks.get(index).cloned().unwrap_or_default();
    card.child(
        div()
            .id(format!("ask-row-{index}"))
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().whitespace_normal().child(format!(
                "{}. {}",
                index + 1,
                row.question
            )))
            .when(!row.choices.is_empty(), |this| {
                this.child(
                    div()
                        .id(format!("ask-choices-{index}"))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .pt_1()
                        .children(
                            row.choices
                                .iter()
                                .enumerate()
                                .map(|(choice_index, choice)| {
                                    let answer = choice.clone();
                                    let multiple = row.multiple;
                                    let selected =
                                        mycode_tools::builtin::ask_choice_selected(&picked, choice);
                                    ask_choice_row(
                                        format!("ask-{index}-{choice_index}-{}", short_id(choice)),
                                        choice.clone(),
                                        selected,
                                        theme,
                                    )
                                    .on_click(cx.listener(
                                        move |workspace, _, window, cx| {
                                            workspace.on_pick_ask_choice(
                                                index,
                                                answer.clone(),
                                                false,
                                                window,
                                                cx,
                                            );
                                            // A single-select pick is the
                                            // answer: step straight on. A
                                            // re-click that un-picks stays.
                                            let picked = workspace
                                                .vm()
                                                .ask_answers
                                                .get(index)
                                                .is_some_and(|answer| !answer.is_empty());
                                            if !multiple && picked {
                                                workspace.on_ask_next(cx);
                                            }
                                        },
                                    ))
                                }),
                        ),
                )
            })
            .when(row.multiple, |this| {
                this.child(div().text_xs().text_color(muted).child(t(
                    "Select more than one, then continue",
                    "可多选，选完后继续",
                )))
            })
            .when(row.optional, |this| {
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
                    .h(px(30.))
                    .text_sm()
                    .child(Input::new(&input)),
            ),
    )
    .child(
        div()
            .id("ask-footer")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .w_full()
            .child(
                ask_ghost_button("ask-decline", t("Decline", "拒绝"), false, theme).on_click(
                    cx.listener(|workspace, _, _, cx| {
                        workspace.on_dismiss_ask(cx);
                    }),
                ),
            )
            .child(div().flex_1().min_w_0())
            .when(index > 0, |this| {
                this.child(
                    ask_ghost_button("ask-back", t("Back", "上一题"), false, theme).on_click(
                        cx.listener(|workspace, _, _, cx| {
                            workspace.on_ask_back(cx);
                        }),
                    ),
                )
            })
            .child(
                ask_ghost_button(
                    "ask-next",
                    if last {
                        t("Answer", "回答")
                    } else {
                        t("Next", "下一题")
                    },
                    true,
                    theme,
                )
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_ask_next(cx);
                })),
            ),
    )
    .into_any_element()
}

/// One ask choice as a full-width ghost row: a hairline border, no fill.
/// The pick shows as an accent edge and label plus a check at the right.
fn ask_choice_row(
    id: impl Into<gpui_kit::ElementId>,
    label: String,
    selected: bool,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let border = if selected {
        crate::ui::desk::primary_edge(theme.primary)
    } else {
        skin::border_subtle(theme)
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .min_h(px(30.))
        .px_3()
        .py_1()
        .rounded(skin::radius_sm())
        .border_1()
        .border_color(border)
        .text_color(if selected {
            theme.primary
        } else {
            theme.foreground
        })
        .text_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(skin::frost_hover(theme)).border_color(border))
        .child(div().flex_1().min_w_0().whitespace_normal().child(label))
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .flex_shrink_0()
                    .text_color(theme.primary),
            )
        })
}

/// A ghost text button: no fill ever, a border only for the primary action.
fn ask_ghost_button(
    id: impl Into<gpui_kit::ElementId>,
    label: &'static str,
    primary: bool,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ink = if primary {
        theme.primary
    } else {
        theme.muted_foreground
    };
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(skin::radius_xs())
        .text_xs()
        .text_color(ink)
        .cursor_pointer()
        .when(primary, |this| {
            this.border_1()
                .border_color(crate::ui::desk::primary_edge(theme.primary))
        })
        .hover(move |style| {
            style
                .bg(skin::frost_hover(theme))
                .text_color(theme.foreground)
        })
        .child(label)
}
