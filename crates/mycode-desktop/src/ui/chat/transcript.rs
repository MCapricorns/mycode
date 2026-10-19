//! The transcript timeline: streaming bubble, committed entries, tool
//! blocks, and user rows with their recall hover action.
use gpui_kit::assets::IconName;
use gpui_kit::component::text::TextView;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, px, relative,
};

use super::markdown;
use crate::i18n::t;
use crate::ui::skin;
use crate::ui::{desk::Desk, ellipsis};
use crate::view_model::{ConversationEntry, EntryKind, StreamingReply};
use crate::workspace::Workspace;

#[derive(Clone, Copy)]
pub(super) struct AssistantDisplay {
    pub reasoning: bool,
    pub speaker: bool,
}

/// The in-flight assistant turn: a live status line, then thinking and text.
///
/// While subagents run the status line only says the turn is waiting for
/// them. Each child's task and cancel sit on its `agent` row in the
/// transcript, and its steps in the inspector and the subagent window.
pub(super) fn render_streaming_entry(
    streaming: &StreamingReply,
    workspace: &Workspace,
    cache: Option<&markdown::MarkdownCache>,
    theme: &Theme,
    display: AssistantDisplay,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let AssistantDisplay {
        reasoning: show_reasoning,
        speaker: show_speaker,
    } = display;
    let desk = Desk::of(theme);
    let working = t("WORKING", "工作中");
    let elapsed = workspace.vm().turn_elapsed_ms();
    let elapsed = if elapsed >= 1_000 {
        format!(" · {}", crate::view_model::elapsed_label(elapsed))
    } else {
        String::new()
    };
    let status = if streaming.status.is_empty() {
        format!("{working}{elapsed}")
    } else {
        format!("{working} · {}{elapsed}", streaming.status)
    };
    let has_text = !streaming.text.trim().is_empty();
    desk_shell(
        div()
            .w_full()
            .min_w_0()
            .h_auto()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .child(markdown::prose_measure(status_line(
                "streaming-status",
                &status,
                desk.amber,
                theme,
            )))
            .when(show_speaker && has_text, |this| {
                this.child(markdown::prose_measure(speaker_row(
                    "streaming-agent-status",
                    t("Agent", "代理"),
                    IconName::Bot,
                    desk.green,
                    theme,
                )))
            })
            .when(show_reasoning && !streaming.thinking.is_empty(), |this| {
                let id: SharedString = "streaming-thinking".into();
                this.child(markdown::prose_measure(thinking_box(
                    id,
                    &streaming.thinking,
                    workspace.thinking_open(),
                    true,
                    theme,
                    cx.listener(|workspace, _, _, cx| {
                        workspace.on_toggle_thinking(cx);
                    }),
                )))
            })
            .when(has_text, |this| {
                this.child(agent_text(
                    "streaming-agent-md".into(),
                    streaming.text.clone().into(),
                    cache,
                    theme,
                    true,
                    window,
                ))
            }),
    )
}

/// Desk transcript entries: a clean timeline block per entry. Assistant text
/// shows one speaker header per turn. User rows and tool blocks arrive
/// pre-routed by the caller's transcript collection and render through their
/// own builders.
pub(super) fn render_entry(
    entry: &ConversationEntry,
    workspace: &Workspace,
    cache: Option<&markdown::MarkdownCache>,
    theme: &Theme,
    display: AssistantDisplay,
    window: &Window,
    cx: &Context<Workspace>,
) -> gpui_kit::AnyElement {
    match entry.kind {
        EntryKind::AssistantMessage => desk_block(
            entry,
            assistant_block(
                entry,
                workspace.thinking_open(),
                display,
                cache,
                theme,
                window,
                cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_thinking(cx);
                }),
            ),
        )
        .into_any_element(),
        EntryKind::ToolResult => {
            let desk = Desk::of(theme);
            let failed = entry.text.starts_with("failed:");
            desk_block(
                entry,
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_normal()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(if failed {
                        desk.red
                    } else {
                        theme.muted_foreground
                    })
                    .child(ellipsis(&entry.text, 600)),
            )
            .into_any_element()
        }
        // Unreachable through the only caller: the transcript collection
        // routes user rows and tool calls to their own builders before any
        // entry reaches this match. The arm only keeps the match total.
        EntryKind::UserMessage | EntryKind::ToolCall => div().into_any_element(),
    }
}

/// Timeline block shell around one committed entry.
fn desk_block(entry: &ConversationEntry, content: impl IntoElement) -> impl IntoElement {
    desk_shell(
        div()
            .id(format!("entry-{}", entry.event_id))
            .w_full()
            .min_w_0()
            .h_auto()
            .flex_none()
            .flex()
            .flex_col()
            .child(content)
            .test_support(),
    )
}

fn desk_shell(content: impl IntoElement) -> impl IntoElement {
    div()
        .group("ledger-row")
        .relative()
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .child(content)
}

/// Avatar plus the speaker's name. The label truncates so a long model name
/// cannot stretch the row.
fn speaker_row(
    id: impl Into<gpui_kit::ElementId>,
    name: &str,
    icon: IconName,
    color: gpui_kit::Hsla,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(skin::space(2))
        .w_full()
        .min_w_0()
        .child(
            div()
                .size(skin::space(5))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(color.opacity(0.14))
                .text_color(color)
                .child(Icon::new(icon).xsmall()),
        )
        .child(
            div()
                .id(id)
                .min_w_0()
                .overflow_hidden()
                .truncate()
                .text_xs()
                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                .text_color(theme.foreground)
                .child(name.to_owned())
                .test_support(),
        )
}

/// A lamp plus a one-line label. The label truncates: a running status
/// carries the whole tool target (a full shell command), which otherwise
/// stretches the row past the transcript column.
fn status_line(
    id: impl Into<gpui_kit::ElementId>,
    label: &str,
    color: gpui_kit::Hsla,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(crate::ui::lamp(color))
        .child(
            div()
                .id(id)
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .truncate()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label.to_owned())
                .test_support(),
        )
}

/// Assistant text plus an optional thinking trace. The trace header and the
/// reply are siblings: closing the trace does not wrap the reply.
fn assistant_block(
    entry: &ConversationEntry,
    open: bool,
    display: AssistantDisplay,
    cache: Option<&markdown::MarkdownCache>,
    theme: &Theme,
    window: &Window,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> impl IntoElement {
    let AssistantDisplay {
        reasoning: show_reasoning,
        speaker: show_speaker,
    } = display;
    let desk = Desk::of(theme);
    let thinking_id: SharedString = format!("thinking-{}", entry.event_id).into();
    let has_text = !entry.text.trim().is_empty();
    let has_thinking = show_reasoning && !entry.thinking.is_empty();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .when(show_speaker && (has_text || has_thinking), |this| {
            this.child(markdown::prose_measure(speaker_row(
                format!("agent-status-{}", entry.event_id),
                t("Agent", "代理"),
                IconName::Bot,
                desk.green,
                theme,
            )))
        })
        .when(has_thinking, |this| {
            this.child(markdown::prose_measure(thinking_box(
                thinking_id,
                &entry.thinking,
                open,
                false,
                theme,
                on_toggle,
            )))
        })
        .when(has_text, |this| {
            this.child(agent_text(
                format!("agent-md-{}", entry.event_id).into(),
                SharedString::from(entry.text.clone()),
                cache,
                theme,
                false,
                window,
            ))
        })
}

/// The transcript column inside the conversation scroller. It fills the chat
/// pane (the strip between the sidebar and Details) instead of a fixed
/// measure shared with the composer.
///
/// `flex_none` keeps the column at the height of its rows. A shrinkable
/// `overflow_hidden` child of the scroller is given a minimum height of zero
/// and compressed to the viewport, so the rows overlap and the reply is
/// clipped to an empty separator.
pub(super) fn transcript_stack() -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id("conversation-inner")
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap_3()
        .py_4()
        .px_4()
}

/// The header row: collapsed, `思考 · <latest line> ▸`; open, the label alone
/// because the trace below already shows every line.
fn thinking_headline(label: &str, open: bool, text: &str) -> String {
    if open {
        return format!("{label} ▾");
    }
    let latest = thinking_latest(text);
    if latest.is_empty() {
        format!("{label} ▸")
    } else {
        format!("{label} · {latest} ▸")
    }
}

/// The newest non-empty line, cut from the front so a line still streaming
/// shows its newest words instead of a fixed opening.
fn thinking_latest(text: &str) -> String {
    const MAX: usize = 42;
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("");
    let count = line.chars().count();
    if count <= MAX {
        return line.to_owned();
    }
    let tail: String = line.chars().skip(count - (MAX - 1)).collect();
    format!("\u{2026}{tail}")
}

/// Thinking trace. Distinct from the answer: a single quiet header row toggles
/// the body. Collapsed, the header stays and the reply (a sibling) does not
/// fold with it.
///
/// The box is content-sized. `overflow_hidden` here lets the scroller treat
/// the row as a zero-minimum flex item and clip it down to a bar.
fn thinking_box(
    id: SharedString,
    text: &str,
    open: bool,
    streaming: bool,
    theme: &Theme,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> impl IntoElement {
    let toggle_id = id.to_string();
    let headline = thinking_headline(t("Thinking", "思考"), open, text);
    let body_id = SharedString::from(format!("thinking-body-{toggle_id}"));
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap(skin::space(1))
        .child(
            div()
                .id(SharedString::from(format!("thinking-toggle-{toggle_id}")))
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .min_w_0()
                .h_auto()
                .flex_none()
                .min_h(px(24.))
                .px(skin::space(1))
                .cursor_pointer()
                .rounded(px(6.))
                .hover(|row| row.bg(theme.secondary_hover.opacity(0.35)))
                .on_click(on_toggle)
                .child(thinking_line(&toggle_id, headline, streaming, theme))
                .test_support(),
        )
        .when(open, |this| {
            this.child(
                div()
                    .id(body_id)
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    .flex_none()
                    .pl(skin::space(3))
                    .border_l_1()
                    .border_color(theme.border.opacity(0.55))
                    .whitespace_normal()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(text.to_owned())
                    .test_support(),
            )
        })
}

fn thinking_line(
    toggle_id: &str,
    headline: String,
    streaming: bool,
    theme: &Theme,
) -> impl IntoElement {
    let label = div()
        .id(SharedString::from(format!("thinking-label-{toggle_id}")))
        .min_w_0()
        .h_auto()
        .flex_none()
        .text_xs()
        .whitespace_nowrap()
        .italic()
        .text_color(theme.magenta)
        .test_support();
    if streaming {
        label.child(
            gpui_kit::component::shimmer::ShimmerText::new(headline)
                .id(SharedString::from(format!("thinking-shimmer-{toggle_id}")))
                .duration(std::time::Duration::from_millis(2400))
                .spread(0.28),
        )
    } else {
        label.child(headline)
    }
}

/// Assistant reply bubble: Markdown via gpui-kit's TextView.
///
/// Fenced code renders in a single default style: the tree-sitter grammars
/// were dropped to halve the binary. The id must be unique per entry —
/// `ElementId::CodeLocation` would collide across blocks since all bubbles
/// render from the same call site.
fn agent_text(
    id: SharedString,
    text: SharedString,
    cache: Option<&markdown::MarkdownCache>,
    theme: &Theme,
    stream_fade: bool,
    window: &Window,
) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_sm()
        .child(markdown::render_markdown(
            id,
            text,
            cache,
            theme,
            stream_fade,
            window,
        ))
}

/// Compaction summary card. The text is the checkpoint summary, shown after
/// both `/compact` and automatic compaction.
pub(super) fn render_summary_entry(entry: &ConversationEntry, theme: &Theme) -> impl IntoElement {
    let body = mycode_app::summary_body(&entry.text);
    div()
        .id(format!("summary-{}", entry.event_id))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(skin::space(1))
        .rounded(px(14.))
        .border_1()
        .border_color(theme.border.opacity(0.65))
        .bg(theme.muted.opacity(0.65))
        .px_3()
        .py_2()
        .child(
            div()
                .text_xs()
                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                .text_color(theme.primary)
                .child(t("SUMMARY", "摘要")),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.foreground)
                .whitespace_normal()
                .child(body.to_owned()),
        )
}

/// Maximum fraction of the prose column a right-aligned user bubble occupies.
pub(super) const USER_BUBBLE_MAX_SHARE: f32 = 0.72;

/// One right-aligned user bubble (capped at 72% width, no speaker header) with
/// one hover action: recall, which rewinds to before this message and puts its
/// text back in the composer for resending. Workspace files are not restored
/// or deleted. Messages without a prior ledger event hide the action.
pub(super) fn render_user_entry(
    entry: &ConversationEntry,
    can_rewind: bool,
    index: usize,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let mut column = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .items_end()
        .gap(skin::space(1))
        .child(
            div()
                .id(format!("user-bubble-{}", entry.event_id))
                .max_w(relative(USER_BUBBLE_MAX_SHARE))
                .min_w_0()
                .rounded(px(14.))
                .bg(theme.secondary)
                .border_1()
                .border_color(theme.border.opacity(0.55))
                .px(skin::space(3))
                .py(skin::space(2))
                .text_sm()
                .whitespace_normal()
                .text_color(theme.foreground)
                .child(TextView::markdown(
                    format!("user-text-{}", entry.event_id),
                    SharedString::from(entry.text.clone()),
                ))
                .test_support(),
        );
    if can_rewind {
        column = column.child(
            div()
                .id(format!("entry-actions-{index}"))
                .flex()
                .flex_row()
                .justify_end()
                .gap_1()
                .opacity(0.0)
                .group_hover("user-entry", |this| this.opacity(1.0))
                .child(
                    div()
                        .id(format!("entry-recall-{index}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .h(px(22.))
                        .rounded(skin::radius_control())
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.bg(skin::frost_hover(theme)))
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.on_recall_message(index, cx);
                        }))
                        .child(Icon::new(IconName::RefreshCcw).xsmall())
                        .child(t("recall", "撤回")),
                )
                .test_support(),
        );
    }
    div()
        .id(format!("entry-{}", entry.event_id))
        .group("user-entry")
        .child(desk_shell(markdown::prose_measure(column)))
        .test_support()
        .into_any_element()
}

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "test-support"))]
mod layout;
