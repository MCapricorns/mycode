//! Shared settings widgets: cards, rows, labeled inputs, dropdowns, and the
//! two-line row header every settings list reuses.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::workspace::Workspace;

/// One settings card: a hairline border, a solid fill, and a mono caption.
pub(super) fn settings_card(
    id: &str,
    title: &str,
    hint: Option<&str>,
    theme: &Theme,
    children: Vec<AnyElement>,
) -> impl IntoElement {
    div()
        .id(format!("card-{id}"))
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(crate::ui::skin::radius_card())
        .border_1()
        .border_color(crate::ui::skin::glass_border(theme))
        .bg(crate::ui::skin::frost_card(theme))
        .child(
            div()
                .id(format!("card-{id}-header"))
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(crate::ui::desk::Desk::of(theme).faint)
                        .child(title.to_owned()),
                )
                .when_some(hint, |this, hint| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .whitespace_normal()
                            .child(hint.to_owned()),
                    )
                }),
        )
        .child(
            div()
                .id(format!("card-{id}-body"))
                .flex()
                .flex_col()
                .gap_2()
                .children(children),
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
/// the General, Agents, Models, and MCP pages; each passes its own toggle
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
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .min_w_0()
                        .child(div().text_sm().child(label.to_owned()))
                        .when_some(description, |this, description| {
                            this.child(div().text_xs().opacity(0.5).child(description.to_owned()))
                        }),
                )
                .child(
                    Button::new(format!("dropdown-button-{id}"))
                        .label(current.to_owned())
                        .icon(IconName::ChevronDown)
                        .small()
                        .outline()
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            on_toggle(workspace, !open, cx);
                        })),
                ),
        )
        .when(open, |this| {
            this.child(
                div()
                    .id(format!("dropdown-list-{id}"))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .p_1()
                    .max_h(px(220.))
                    .overflow_y_scroll()
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
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .px_2()
                            .h(px(28.))
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
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
