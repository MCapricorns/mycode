//! Shared settings widgets: cards, rows, labeled inputs, dropdowns, and the
//! two-line row header every settings list reuses.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};

use crate::workspace::Workspace;

/// One settings sheet with a clear heading and aligned control rows.
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
                    this.mt(crate::ui::skin::space(4))
                        .pt(crate::ui::skin::space(4))
                        .border_t_1()
                        .border_color(theme.border)
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
        .gap(crate::ui::skin::space(4))
        .p(crate::ui::skin::space(5))
        .rounded(crate::ui::skin::radius_card())
        .border_1()
        .border_color(crate::ui::skin::glass_border(theme))
        .bg(crate::ui::skin::panel_fill(theme))
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
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
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
    theme: &Theme,
) -> AnyElement {
    let muted = theme.muted_foreground;
    div()
        .id(format!("row-{id}"))
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .test_support()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex()
                .id(format!("row-{id}-label"))
                .flex_col()
                .gap_0p5()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().whitespace_normal().child(label.to_owned()))
                .when_some(description, |this, description| {
                    this.child(
                        div()
                            .id(format!("row-{id}-hint"))
                            .test_support()
                            .text_xs()
                            .text_color(muted)
                            .whitespace_normal()
                            .child(description.to_owned()),
                    )
                }),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// A segmented control: one recessed track with the picked option raised
/// on it. Every option is visibly part of the control, so an unpicked
/// choice does not read as plain text. A long list wraps inside the track.
pub(super) fn choice_chips<V: AsRef<str>, L: AsRef<str>>(
    id_prefix: &str,
    options: &[(V, L)],
    current: &str,
    on_pick: impl Fn(&mut Workspace, &str, &mut Context<Workspace>) + Clone + 'static,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    choice_track(id_prefix, theme)
        .children(options.iter().map(|(value, label)| {
            let value = value.as_ref().to_owned();
            let selected = value == current;
            let pick = on_pick.clone();
            choice_chip(id_prefix, &value, label.as_ref(), selected, theme).when(
                !selected,
                |this| {
                    this.on_click(cx.listener(move |workspace, _, _, cx| {
                        pick(workspace, &value, cx);
                    }))
                },
            )
        }))
        .into_any_element()
}

pub(super) fn choice_track(id_prefix: &str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(format!("chips-{id_prefix}"))
        .max_w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .self_start()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .content_start()
        .justify_start()
        .gap(px(2.))
        .p(px(2.))
        .rounded(crate::ui::skin::radius_control())
        .border_1()
        .border_color(crate::ui::skin::glass_border(theme))
        .bg(crate::ui::skin::segment_track(theme))
}

pub(super) fn choice_chip(
    id_prefix: &str,
    value: &str,
    label: &str,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.secondary_hover;
    let ink = theme.foreground;
    div()
        .id(format!("{id_prefix}-{value}"))
        .min_h(px(28.))
        .min_w(px(40.))
        .px(crate::ui::skin::space(3))
        .py(px(3.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(crate::ui::skin::radius_control())
        .border_1()
        .when(selected, |this| this.border_b_2())
        .border_color(if selected {
            theme.border
        } else {
            theme.transparent
        })
        .text_sm()
        .whitespace_nowrap()
        .when(selected, |this| {
            this.bg(crate::ui::skin::segment_thumb(theme))
                .text_color(ink)
        })
        .when(!selected, |this| {
            this.text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(move |this| this.bg(hover).text_color(ink))
        })
        .child(label.to_owned())
}

/// Value control shared by dropdown rows and their layout checks.
pub(super) fn dropdown_button(id: &str, current: &str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(format!("dropdown-button-{id}"))
        .min_h(px(32.))
        .w(px(200.))
        .flex_none()
        .px_2()
        .py_1()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .rounded(crate::ui::skin::radius_control())
        .border_1()
        .border_b_2()
        .border_color(theme.border)
        .bg(theme.secondary)
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary_hover))
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_sm()
                .child(current.to_owned()),
        )
        .child(
            Icon::new(IconName::ChevronDown)
                .xsmall()
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        )
}

/// The data side of a [`dropdown_field`]: what the row shows and which
/// option is current.
pub(super) struct DropdownSpec<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub description: Option<&'a str>,
    pub current: &'a str,
    pub options: &'a [String],
    pub open: bool,
}

/// A dropdown row: label on the left, a button showing the current value on
/// the right, and an inline option list that opens below the row. Shared by
/// the General, Models, and MCP pages; each passes its own toggle and pick
/// closures.
pub(super) fn dropdown_field(
    spec: DropdownSpec<'_>,
    on_toggle: impl Fn(&mut Workspace, bool, &mut Context<Workspace>) + 'static,
    on_pick: impl Fn(&mut Workspace, &str, &mut Context<Workspace>) + Clone + 'static,
    cx: &Context<Workspace>,
) -> AnyElement {
    let DropdownSpec {
        id,
        label,
        description,
        current,
        options,
        open,
    } = spec;
    let theme = cx.theme();
    div()
        .id(format!("dropdown-{id}"))
        .w_full()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .child(settings_row(
            &format!("dropdown-{id}"),
            label,
            description,
            dropdown_button(id, current, theme)
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    on_toggle(workspace, !open, cx);
                }))
                .into_any_element(),
            theme,
        ))
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
