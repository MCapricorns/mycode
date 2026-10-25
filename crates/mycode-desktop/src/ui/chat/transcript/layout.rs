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
                        |_, _, _| {},
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
fn user_bubbles_align_right_stay_within_72_percent_and_omit_ledger_stamps(cx: &mut TestAppContext) {
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
                                    .child(super::desk_shell(super::markdown::prose_measure(
                                        column,
                                    )))
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
