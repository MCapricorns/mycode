//! Structured questions, docked right above the composer like Devin's ask
//! card. One question shows at a time: a single-select pick advances to the
//! next, a multi-select or free text waits for Next, and the last answer
//! submits. The card is accent-tinted glass over the window blur; choices
//! are frosted rows with a radio or check mark, and the answer action uses
//! the shared tinted primary button.
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Input;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px, relative,
};

use crate::i18n::t;
use crate::ui::{desk::Desk, short_id, skin};
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
        .gap_3()
        .px_4()
        .pt_3()
        .pb(skin::space(3))
        // Matches the composer card under it, so the pair reads as one stack.
        .rounded(px(18.))
        .bg(skin::ask_glass(theme))
        .border_color(crate::ui::desk::primary_edge(theme.primary).opacity(0.35))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size(skin::space(6))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(desk.violet.opacity(0.16))
                        .text_color(desk.violet)
                        .child(Icon::new(IconName::MessageSquare).xsmall()),
                )
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
        )
        .when(total > 1, |this| {
            this.child(ask_progress(index, total, theme))
        });
    let (Some(row), Some(input)) = (rows.get(index).cloned(), input) else {
        return card.into_any_element();
    };
    let picked = picks.get(index).cloned().unwrap_or_default();
    let hint = match (row.multiple, row.optional) {
        (true, true) => Some(t("Select any · optional", "可多选 · 可选")),
        (true, false) => Some(t(
            "Select more than one, then continue",
            "可多选，选完后继续",
        )),
        (false, true) => Some(t("Optional", "可选")),
        (false, false) => None,
    };
    card.child(
        div()
            .id(format!("ask-row-{index}"))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(crate::ui::skin::WEIGHT_STRONG)
                    .whitespace_normal()
                    .child(row.question.clone()),
            )
            .when_some(hint, |this, hint| {
                this.child(div().text_xs().text_color(muted).child(hint))
            })
            .when(!row.choices.is_empty(), |this| {
                this.child(
                    div()
                        .id(format!("ask-choices-{index}"))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
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
                                        multiple,
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
            .child(
                div()
                    .id(format!("ask-free-input-{index}"))
                    .w_full()
                    .min_w_0()
                    .h(px(32.))
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
            .pt(skin::space(3))
            .border_t_1()
            .border_color(skin::border_subtle(theme))
            .child(
                ask_text_button("ask-decline", t("Decline", "拒绝"), theme).on_click(cx.listener(
                    |workspace, _, _, cx| {
                        workspace.on_dismiss_ask(cx);
                    },
                )),
            )
            .child(div().flex_1().min_w_0())
            .when(index > 0, |this| {
                this.child(
                    skin::glass_button("ask-back", false, theme)
                        .h(px(28.))
                        .text_xs()
                        .child(Icon::new(IconName::ArrowLeft).xsmall())
                        .child(t("Back", "上一题"))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_ask_back(cx);
                        })),
                )
            })
            .child(
                skin::glass_button("ask-next", true, theme)
                    .h(px(28.))
                    .text_xs()
                    .font_weight(crate::ui::skin::WEIGHT_STRONG)
                    .child(if last {
                        t("Answer", "回答")
                    } else {
                        t("Next", "下一题")
                    })
                    .child(
                        Icon::new(if last {
                            IconName::CornerDownLeft
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    )
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_ask_next(cx);
                    })),
            ),
    )
    .into_any_element()
}

/// A thin segmented bar: answered and current questions in the accent,
/// the rest as a faint track.
fn ask_progress(index: usize, total: usize, theme: &Theme) -> impl IntoElement {
    div()
        .id("ask-progress")
        .flex()
        .flex_row()
        .gap(px(4.))
        .w_full()
        .children((0..total).map(|step| {
            let fill = if step == index {
                theme.primary.opacity(0.9)
            } else if step < index {
                theme.primary.opacity(0.45)
            } else {
                skin::border_strong(theme)
            };
            div().flex_1().h(px(3.)).rounded_full().bg(fill)
        }))
}

/// One ask choice as a full-width frosted row with a radio (single) or
/// check (multiple) mark on the left. The pick takes the accent tint and
/// edge.
fn ask_choice_row(
    id: impl Into<gpui_kit::ElementId>,
    label: String,
    selected: bool,
    multiple: bool,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let border = if selected {
        crate::ui::desk::primary_edge(theme.primary)
    } else {
        skin::border_subtle(theme)
    };
    let (fill, hover) = if selected {
        (theme.accent.opacity(0.85), theme.accent)
    } else {
        (skin::frost(theme), skin::frost_hover(theme))
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(skin::space(3))
        .w_full()
        .min_h(px(34.))
        .px_3()
        .py(px(6.))
        .rounded(skin::radius_sm())
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_color(theme.foreground)
        .text_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(hover).border_color(border))
        .child(choice_mark(selected, multiple, theme))
        .child(div().flex_1().min_w_0().whitespace_normal().child(label))
}

/// A radio dot or a checkbox tick in front of a choice label.
fn choice_mark(selected: bool, multiple: bool, theme: &Theme) -> gpui_kit::AnyElement {
    let edge = if selected {
        theme.primary
    } else {
        skin::border_strong(theme)
    };
    let mark = div()
        .size(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(edge);
    if multiple {
        mark.rounded(px(4.))
            .when(selected, |this| {
                this.bg(theme.primary).child(
                    Icon::new(IconName::Check)
                        .xsmall()
                        .text_color(theme.primary_foreground),
                )
            })
            .into_any_element()
    } else {
        mark.rounded_full()
            .when(selected, |this| {
                this.child(div().size(relative(0.5)).rounded_full().bg(theme.primary))
            })
            .into_any_element()
    }
}

/// Decline: muted text that only lifts on hover, so the card has one clear
/// primary action.
fn ask_text_button(
    id: impl Into<gpui_kit::ElementId>,
    label: &'static str,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(skin::radius_xs())
        .text_xs()
        .text_color(theme.muted_foreground)
        .cursor_pointer()
        .hover(move |style| {
            style
                .bg(skin::frost_hover(theme))
                .text_color(theme.foreground)
        })
        .child(label)
}
