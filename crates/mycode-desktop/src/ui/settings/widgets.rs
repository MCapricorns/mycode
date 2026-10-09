//! Shared settings widgets: cards, rows, labeled inputs, dropdowns, and the
//! two-line row header every settings list reuses.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};

use crate::workspace::Workspace;

/// One settings card: a hairline border, a frosted fill, and a short title.
pub(super) fn settings_card(
    id: &str,
    title: &str,
    hint: Option<&str>,
    theme: &Theme,
    children: Vec<AnyElement>,
) -> impl IntoElement {
    let body = children
        .into_iter()
        .enumerate()
        .map(|(index, child)| {
            // Content height only. A growing section becomes the settings
            // scrollport and leaves a blank band above the next control.
            // Flex column, not a block. A block section whose height is
            // already known skips measuring its children and can keep the
            // scrollport height while the controls paint nothing.
            div()
                .w_full()
                .min_w_0()
                .h_auto()
                .flex_none()
                .flex()
                .flex_col()
                .justify_start()
                .when(index > 0, |this| {
                    this.mt_3().pt_3().border_t_1().border_color(theme.border)
                })
                .child(child)
        })
        .collect::<Vec<_>>();
    div()
        .id(format!("card-{id}"))
        // Content height. A card that grows into the settings scrollport
        // leaves a blank band between the sections inside it.
        .w_full()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .justify_start()
        .gap_3()
        .p(px(20.))
        .rounded(crate::ui::skin::radius_card())
        .border_1()
        .border_color(crate::ui::skin::glass_border(theme))
        .bg(crate::ui::skin::frost_card(theme))
        .test_support()
        .child(
            div()
                .id(format!("card-{id}-header"))
                .w_full()
                .h_auto()
                .flex_none()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(title.to_owned()),
                )
                .when_some(hint, |this, hint| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .whitespace_normal()
                            .child(hint.to_owned()),
                    )
                }),
        )
        .child(
            div()
                .id(format!("card-{id}-body"))
                .w_full()
                .h_auto()
                .flex_none()
                .flex()
                .flex_col()
                .justify_start()
                .children(body),
        )
}

/// A label + control row used across sections.
pub(super) fn settings_row(
    id: &str,
    label: &str,
    description: Option<&str>,
    control: AnyElement,
) -> AnyElement {
    div()
        .id(format!("row-{id}"))
        .flex()
        .flex_row()
        .items_start()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().whitespace_normal().child(label.to_owned()))
                .when_some(description, |this, description| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .whitespace_normal()
                            .child(description.to_owned()),
                    )
                }),
        )
        .child(control)
        .into_any_element()
}

/// Wrapping choice chips. The label stays in the parent column so a long
/// label cannot collapse beside the chips.
pub(super) fn choice_chips(
    id_prefix: &str,
    options: &[(String, String)],
    current: &str,
    on_pick: impl Fn(&mut Workspace, &str, &mut Context<Workspace>) + Clone + 'static,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id(format!("chips-{id_prefix}"))
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_start()
        .content_start()
        .justify_start()
        .gap_1()
        .children(options.iter().map(|(value, label)| {
            let value = value.clone();
            let selected = value == current;
            let pick = on_pick.clone();
            div()
                .id(format!("{id_prefix}-{value}"))
                .h(px(28.))
                .px_2()
                .flex_none()
                .self_start()
                .flex()
                .items_center()
                .rounded(px(8.))
                .border_1()
                .border_color(if selected {
                    theme.primary
                } else {
                    theme.border
                })
                .when(selected, |this| {
                    this.bg(crate::ui::skin::frost_accent(theme))
                })
                .text_sm()
                .cursor_pointer()
                .hover(|this| this.bg(crate::ui::skin::frost_hover(theme)))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    pick(workspace, &value, cx);
                }))
                .child(label.clone())
        }))
        .into_any_element()
}

/// The two-line header column shared by settings rows: a medium title over a
/// dim subtitle. Returns the bare column so callers can append extra lines.
pub(super) fn row_header(title: &str, subtitle: String) -> gpui_kit::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .overflow_hidden()
                .child(title.to_owned()),
        )
        .child(
            div()
                .text_xs()
                .opacity(0.6)
                .whitespace_normal()
                .child(subtitle),
        )
}

pub(super) fn labeled_field(label: &str, input: Entity<InputState>) -> impl IntoElement {
    div()
        .id(format!("form-field-{label}"))
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().opacity(0.6).child(label.to_owned()))
        .child(div().h(px(28.)).child(Input::new(&input)))
}

/// A dropdown row: label on the left, a button showing the current value on
/// the right, and an inline option list that opens below the row. Shared by
/// the General, Shell, Agents, Models, and MCP pages; each passes its own toggle
/// and pick closures.
#[allow(clippy::too_many_arguments)]
pub(super) fn dropdown_field(
    id: &str,
    label: &str,
    description: Option<&str>,
    current: &str,
    options: &[String],
    open: bool,
    on_toggle: impl Fn(&mut Workspace, bool, &mut Context<Workspace>) + 'static,
    on_pick: impl Fn(&mut Workspace, &str, &mut Context<Workspace>) + Clone + 'static,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id(format!("dropdown-{id}"))
        .w_full()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            // The row is as wide as the card. The label column is `min_w_0`
            // so a long helper wraps inside the card instead of stretching
            // the row and pushing the control past the right edge. The row
            // itself stays `w_full`: a shrinkable row with no width lets the
            // label measure at width 0 inside the settings scroller, and
            // `white-space: normal` then stacks one glyph per line.
            div()
                .id(format!("dropdown-row-{id}"))
                .w_full()
                .min_w_0()
                .h_auto()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_auto()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .text_sm()
                                .whitespace_normal()
                                .child(label.to_owned()),
                        )
                        .when_some(description, |this, description| {
                            this.child(
                                div()
                                    .id(format!("dropdown-hint-{id}"))
                                    .w_full()
                                    .min_w_0()
                                    .text_xs()
                                    .opacity(0.5)
                                    .whitespace_normal()
                                    .child(description.to_owned()),
                            )
                        }),
                )
                .child(
                    div()
                        .id(format!("dropdown-button-{id}"))
                        .h(px(32.))
                        .w(px(200.))
                        .max_w(px(200.))
                        .flex_none()
                        .flex_shrink_0()
                        .px_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .rounded(crate::ui::skin::radius_control())
                        .border_1()
                        .border_color(crate::ui::skin::glass_border(theme))
                        .bg(crate::ui::skin::frost(theme))
                        .cursor_pointer()
                        .hover(|this| this.bg(crate::ui::skin::frost_hover(theme)))
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            on_toggle(workspace, !open, cx);
                        }))
                        .child(
                            // No `truncate`: overflow hidden on a `flex_1`
                            // child inside this scroller is what collapses
                            // the value (and then the row) on real windows.
                            div()
                                .flex_none()
                                .whitespace_nowrap()
                                .text_sm()
                                .child(current.to_owned()),
                        )
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .xsmall()
                                .text_color(theme.muted_foreground),
                        ),
                ),
        )
        .when(open, |this| {
            this.child(
                div()
                    .id(format!("dropdown-list-{id}"))
                    .w_full()
                    .h_auto()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_start()
                    .gap_0p5()
                    .p_1()
                    .rounded(crate::ui::skin::radius_control())
                    .border_1()
                    .border_color(crate::ui::skin::glass_border(theme))
                    .bg(crate::ui::skin::frost_card(theme))
                    .children(options.iter().map(|option| {
                        let option = option.clone();
                        let row_option = option.clone();
                        let selected = option == current;
                        let pick = on_pick.clone();
                        div()
                            .id(format!("dropdown-{id}-{option}"))
                            .w_full()
                            .h(px(28.))
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .px_2()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .when(selected, |this| {
                                this.bg(crate::ui::skin::frost_accent(theme))
                            })
                            .hover(|this| this.bg(crate::ui::skin::frost_hover(theme)))
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                pick(workspace, &row_option, cx);
                            }))
                            .child(option)
                            .when(selected, |this| {
                                this.child(
                                    Icon::new(IconName::Check)
                                        .xsmall()
                                        .text_color(theme.primary),
                                )
                            })
                    })),
            )
        })
        .into_any_element()
}
