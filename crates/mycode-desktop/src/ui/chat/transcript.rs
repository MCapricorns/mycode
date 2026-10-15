//! The transcript timeline: streaming bubble, committed entries, tool
//! blocks, and user rows with their edit/recall hover actions.
use gpui_kit::assets::IconName;
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
/// Subagent progress lives on this one status line, the running list above
/// the composer, and the inspector panel. The transcript itself stays free
/// of per-child rows.
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
    let status = if streaming.status.is_empty() {
        t("Working", "工作中").to_owned()
    } else {
        streaming.status.clone()
    };
    let has_text = !streaming.text.trim().is_empty();
    desk_shell(
        "streaming",
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
                &format!("{} · {status}", t("WORKING", "工作中")),
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
            })
            .when(streaming.thinking.trim().is_empty() && !has_text, |this| {
                this.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .truncate()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(status),
                )
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
        &entry.event_id,
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

fn desk_shell(_id: &str, content: impl IntoElement) -> impl IntoElement {
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
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
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
        .text_color(theme.muted_foreground)
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
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
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
/// hover actions: edit-and-resend (rewinds to before this message and prefills
/// the composer) and recall (drops this message and everything after). Neither
/// action restores or deletes workspace files. Messages without a prior ledger
/// event hide these actions.
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
                .child(SharedString::from(entry.text.clone()))
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
                        .id(format!("entry-edit-{index}"))
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
                            workspace.on_edit_message(index, cx);
                        }))
                        .child(Icon::new(IconName::Pen).xsmall())
                        .child(t("edit", "编辑")),
                )
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
        .child(desk_shell(&entry.event_id, markdown::prose_measure(column)))
        .test_support()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn collapsed_thinking_headline_follows_the_newest_line() {
        let trace = "reasoning step 0 weighs the next edit\nsecond line\n\n";
        assert_eq!(
            super::thinking_headline("思考", false, trace),
            "思考 · second line ▸"
        );
        assert_eq!(
            super::thinking_headline("Thinking", true, trace),
            "Thinking ▾"
        );
        assert_eq!(super::thinking_headline("思考", false, "   "), "思考 ▸");
        let long = "a streaming line that keeps growing with the newest words at the end";
        let headline = super::thinking_headline("思考", false, long);
        assert!(
            headline.ends_with("newest words at the end ▸"),
            "{headline}"
        );
        assert!(headline.contains('\u{2026}'), "{headline}");
    }
}

#[cfg(all(test, feature = "test-support"))]
mod layout {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    use gpui_kit::component::ActiveTheme as _;
    use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
    use gpui_kit::{
        AppContext as _, Context, InteractiveElement, IntoElement, ParentElement, Render,
        StatefulInteractiveElement, Styled, TestAppContext, Window, div, px, size,
    };

    use super::{assistant_block, desk_block, transcript_stack};
    use crate::view_model::{ConversationEntry, EntryKind};

    struct TranscriptProbe {
        viewport: f32,
        entries: Vec<ConversationEntry>,
        open: Rc<RefCell<BTreeSet<String>>>,
    }

    fn assistant(id: &str, text: &str) -> ConversationEntry {
        let thinking = (0..40)
            .map(|n| format!("reasoning step {n} weighs the next edit"))
            .collect::<Vec<_>>()
            .join("\n");
        ConversationEntry {
            event_id: id.into(),
            kind: EntryKind::AssistantMessage,
            text: text.into(),
            call_id: None,
            thinking,
        }
    }

    impl Render for TranscriptProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme().clone();
            let open = self.open.borrow().clone();
            let rows: Vec<gpui_kit::AnyElement> = self
                .entries
                .iter()
                .map(|entry| {
                    let key = format!("thinking-{}", entry.event_id);
                    let expanded = open.contains(&key);
                    let toggle_key = key.clone();
                    desk_block(
                        entry,
                        assistant_block(
                            entry,
                            expanded,
                            super::AssistantDisplay {
                                reasoning: true,
                                speaker: true,
                            },
                            None,
                            &theme,
                            window,
                            cx.listener(move |probe, _, _, cx| {
                                {
                                    let mut open = probe.open.borrow_mut();
                                    if !open.remove(&toggle_key) {
                                        open.insert(toggle_key.clone());
                                    }
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .into_any_element()
                })
                .collect();
            div().size_full().child(
                div()
                    .id("conversation")
                    .w(px(760.))
                    .h(px(self.viewport))
                    .overflow_x_hidden()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(transcript_stack().children(rows).test_support())
                    .test_support(),
            )
        }
    }

    fn open_probe(
        cx: &mut TestAppContext,
        viewport: f32,
        count: usize,
    ) -> (
        gpui_kit::WindowHandle<TranscriptProbe>,
        Rc<RefCell<BTreeSet<String>>>,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let entries = (1..=count)
            .map(|n| {
                assistant(
                    &format!("e{n}"),
                    &format!("Visible reply {n} stays in the transcript."),
                )
            })
            .collect();
        let open = Rc::new(RefCell::new(BTreeSet::new()));
        let probe_open = open.clone();
        let handle = cx.open_window(size(px(900.), px(1400.)), move |_, _| TranscriptProbe {
            viewport,
            entries,
            open: probe_open,
        });
        (handle, open)
    }

    #[gpui_kit::test]
    fn a_long_running_status_stays_inside_the_column(cx: &mut TestAppContext) {
        struct StatusProbe(String);
        impl Render for StatusProbe {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = cx.theme().clone();
                div().size_full().child(
                    div()
                        .id("conversation")
                        .w(px(760.))
                        .h(px(200.))
                        .overflow_x_hidden()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .id("status-column")
                                .w_full()
                                .px(px(16.))
                                .child(super::status_line(
                                    "probe-status",
                                    &self.0,
                                    gpui_kit::white(),
                                    &theme,
                                ))
                                .test_support(),
                        )
                        .test_support(),
                )
            }
        }
        // One tool status carrying a full shell command, far wider than the
        // 760px column. QA saw this line run off the right window edge.
        let command = format!(
            "正在执行 shell git stash push -m \"{}\"",
            "wip: local edits before sync ".repeat(30)
        );
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(900.), px(400.)), move |_, _| StatusProbe(command));
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let scroller = window.find("conversation").bounds();
            let label = window.find("probe-status").bounds();
            assert!(
                label.right() <= scroller.right() + px(1.),
                "a running status must truncate inside the transcript column: label {label:?} scroller {scroller:?}"
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn collapsed_thinking_keeps_one_header_and_the_reply(cx: &mut TestAppContext) {
        let (handle, open) = open_probe(cx, 1200., 1);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let header = window.find("thinking-toggle-thinking-e1");
            let reply = window.find("reply-shell-agent-md-e1");
            let entry = window.find("entry-e1").bounds();
            let header_bounds = header.bounds();
            let reply_bounds = reply.bounds();
            assert!(
                window.try_find("thinking-body-thinking-e1").is_none(),
                "collapsed thinking must not mount the trace"
            );
            assert!(
                header.visible()
                    && header_bounds.size.height >= px(16.)
                    && header_bounds.size.height <= px(40.)
                    && header_bounds.size.width >= px(48.),
                "collapsed thinking should be one header row, got {header_bounds:?} visible {}",
                header.visible()
            );
            assert!(
                reply.visible()
                    && reply_bounds.size.height >= px(14.)
                    && reply_bounds.size.height <= px(90.)
                    && reply_bounds.size.width >= px(80.)
                    && reply_bounds.origin.y >= header_bounds.bottom()
                    && reply_bounds.origin.y - header_bounds.bottom() < px(80.),
                "the reply stays under the header and does not collapse with it: header {header_bounds:?} reply {reply_bounds:?} visible {}",
                reply.visible()
            );
            assert!(
                entry.size.height >= px(48.) && entry.size.height <= px(200.),
                "a collapsed reply should be a short row, not an empty line or the whole trace: {entry:?}"
            );

            window.click("thinking-toggle-thinking-e1", cx);
            window.render_frame(cx);
            assert!(open.borrow().contains("thinking-e1"));
            let body = window.find("thinking-body-thinking-e1");
            let header = window.find("thinking-toggle-thinking-e1").bounds();
            let reply = window.find("reply-shell-agent-md-e1");
            let body_bounds = body.bounds();
            let reply_bounds = reply.bounds();
            let expanded = window.find("entry-e1").bounds();
            assert!(
                body.visible()
                    && body_bounds.origin.y >= header.bottom()
                    && body_bounds.size.height >= px(80.),
                "opening the header reveals the trace under it: header {header:?} body {body_bounds:?}"
            );
            assert!(
                reply.visible()
                    && reply_bounds.origin.y >= body_bounds.bottom()
                    && reply_bounds.size.height >= px(14.),
                "the reply stays below the opened trace: body {body_bounds:?} reply {reply_bounds:?}"
            );
            assert!(
                expanded.size.height > entry.size.height + px(60.),
                "opening thinking grows the row below the collapsed height: collapsed {entry:?} expanded {expanded:?}"
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn collapsed_replies_do_not_shrink_into_separator_lines(cx: &mut TestAppContext) {
        let (handle, _) = open_probe(cx, 150., 6);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let scroller = window.find("conversation").bounds();
            let stack = window
                .find("conversation-inner")
                .bounds();
            assert!(
                stack.size.height > scroller.size.height + px(40.),
                "the transcript column should grow past the viewport instead of shrinking into it: stack {stack:?} scroller {scroller:?}"
            );
            let mut previous_bottom = scroller.origin.y;
            for n in 1..=6 {
                let id = format!("e{n}");
                let entry = window.find(format!("entry-{id}")).bounds();
                let header = window.find(format!("thinking-toggle-thinking-{id}"));
                let reply = window.find(format!("reply-shell-agent-md-{id}"));
                let header_bounds = header.bounds();
                let reply_bounds = reply.bounds();
                assert!(
                    window
                        .try_find(format!("thinking-body-thinking-{id}"))
                        .is_none(),
                    "entry {id} should stay collapsed"
                );
                assert!(
                    entry.size.height >= px(48.) && entry.size.height <= px(200.),
                    "entry {id} collapsed into an empty separator: {entry:?} stack {stack:?}"
                );
                assert!(
                    header_bounds.size.height >= px(16.)
                        && header_bounds.size.height <= px(40.)
                        && reply_bounds.size.height >= px(14.)
                        && reply_bounds.size.height <= px(90.)
                        && reply_bounds.origin.y >= header_bounds.bottom(),
                    "entry {id} hid its header or reply: header {header_bounds:?} reply {reply_bounds:?}"
                );
                assert!(
                    entry.origin.y >= previous_bottom - px(1.),
                    "entry {id} overlaps the previous row: {entry:?} after {previous_bottom:?}"
                );
                let fully_on_screen = header_bounds.origin.y >= scroller.origin.y
                    && reply_bounds.bottom() <= scroller.bottom();
                if fully_on_screen {
                    assert!(
                        header.visible() && reply.visible(),
                        "on-screen entry {id} clipped its header or reply: header {header_bounds:?} reply {reply_bounds:?}"
                    );
                }
                previous_bottom = entry.bottom();
            }
            let first_reply = window.find("reply-shell-agent-md-e1");
            assert!(
                first_reply.visible()
                    && first_reply.bounds().size.height >= px(14.)
                    && window.find("thinking-toggle-thinking-e1").visible(),
                "the first collapsed reply should paint its header and text, not an empty line: {:?}",
                first_reply.bounds()
            );
            let last = window.find("entry-e6").bounds();
            assert!(
                last.bottom() > scroller.bottom() + px(8.),
                "rows should keep their height and scroll, not shrink into the viewport: last {last:?} scroller {scroller:?}"
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn user_bubbles_align_right_stay_within_72_percent_and_omit_ledger_stamps(
        cx: &mut TestAppContext,
    ) {
        struct UserProbe(ConversationEntry);
        impl Render for UserProbe {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = cx.theme().clone();
                let column = div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_end()
                    .child(
                        div()
                            .id(format!("user-bubble-{}", self.0.event_id))
                            .max_w(gpui_kit::relative(super::USER_BUBBLE_MAX_SHARE))
                            .min_w_0()
                            .rounded(px(14.))
                            .bg(theme.secondary)
                            .px(px(12.))
                            .py(px(8.))
                            .text_sm()
                            .whitespace_normal()
                            .child(gpui_kit::SharedString::from(self.0.text.clone()))
                            .test_support(),
                    );
                div().size_full().child(
                    div()
                        .id("conversation")
                        .w(px(800.))
                        .h(px(300.))
                        .child(
                            super::transcript_stack()
                                .child(
                                    div()
                                        .id(format!("entry-{}", self.0.event_id))
                                        .child(super::desk_shell(
                                            &self.0.event_id,
                                            super::markdown::prose_measure(column),
                                        ))
                                        .test_support(),
                                )
                                .test_support(),
                        )
                        .test_support(),
                )
            }
        }
        let entry = ConversationEntry {
            event_id: "u1".into(),
            kind: EntryKind::UserMessage,
            text: "A long user prompt that wraps across multiple lines inside the right-aligned bubble without stretching across the full transcript width."
                .repeat(3)
                .into(),
            call_id: None,
            thinking: String::new(),
        };
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(900.), px(400.)), move |_, _| UserProbe(entry));
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("ledger-stamp-u1").is_none(),
                "ledger stamps must not be mounted in the transcript"
            );
            assert!(
                window.try_find("you-status-u1").is_none(),
                "user rows must not paint a You speaker header"
            );
            let row = window.find("entry-u1").bounds();
            let bubble = window.find("user-bubble-u1").bounds();
            let max_allowed = f32::from(row.size.width) * super::USER_BUBBLE_MAX_SHARE + 2.;
            assert!(
                f32::from(bubble.size.width) <= max_allowed,
                "user bubble width {:?} must not exceed 72% of row {:?}",
                bubble.size.width,
                row.size.width
            );
            assert!(
                (row.right() - bubble.right()).abs() <= px(2.),
                "user bubble must align to the right edge of its row: bubble {bubble:?} row {row:?}"
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn assistant_speaker_header_renders_only_when_requested_for_the_turn(cx: &mut TestAppContext) {
        struct TurnProbe(Vec<(ConversationEntry, bool)>);
        impl Render for TurnProbe {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = cx.theme().clone();
                let rows = self.0.iter().map(|(entry, show_speaker)| {
                    desk_block(
                        entry,
                        assistant_block(
                            entry,
                            false,
                            super::AssistantDisplay {
                                reasoning: true,
                                speaker: *show_speaker,
                            },
                            None,
                            &theme,
                            window,
                            |_, _, _| {},
                        ),
                    )
                });
                div().size_full().child(
                    div()
                        .id("conversation")
                        .w(px(760.))
                        .h(px(400.))
                        .child(transcript_stack().children(rows).test_support())
                        .test_support(),
                )
            }
        }
        let first = assistant("step-1", "First assistant step in this turn.");
        let second = assistant("step-2", "Second assistant step in the same turn.");
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let handle = cx.open_window(size(px(900.), px(500.)), move |_, _| {
            TurnProbe(vec![(first, true), (second, false)])
        });
        cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("agent-status-step-1").is_some(),
            "first assistant step in a turn must render the Agent speaker header"
        );
        assert!(
            window.try_find("agent-status-step-2").is_none(),
            "subsequent assistant steps in the same turn must not repeat the Agent speaker header"
        );
    })
    .unwrap();
    }
}
