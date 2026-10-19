//! The chat column: conversation transcript and the integrated composer.
//! The pending-ask card docks inside the composer; both fill the strip
//! between the sidebar and Details.
mod ask;
mod composer;
mod markdown;
mod menus;
mod scroll_hold;
mod tools;
mod transcript;
mod welcome;

pub(crate) use composer::composer_placeholder;
pub(crate) use menus::reasoning_row_label;
pub(crate) use tools::parse_diff_hunk_header;
pub(crate) use welcome::new_task_label;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use crate::i18n::t;
use crate::view_model::{ConversationEntry, EntryKind};
use crate::workspace::Workspace;

pub(super) fn render_chat(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    // Entries render straight from state: cloning the whole transcript per
    // frame made every notify (menu toggles, stream deltas) allocate all the
    // message text again. The borrow is scoped so the welcome and composer
    // builders can still take `&mut Workspace`.
    let sending = workspace.vm().sending;
    let loading_older = workspace.vm().history_loading;
    // Off means the user asked not to see a reasoning trace. The text stays
    // in the ledger; only the thinking header and its trace are hidden while
    // that pick is set.
    let show_reasoning = crate::view_model::selected_reasoning_level(workspace.vm()) != "off";
    let (show_welcome, has_older, entry_elements, streaming_element) = {
        let md_cache = scroll_hold::use_markdown_cache(window);
        let active = workspace.vm().active.as_ref();
        let entries: &[ConversationEntry] = active
            .map(|conversation| conversation.entries.as_slice())
            .unwrap_or_default();
        let streaming = active.and_then(|c| c.streaming.as_ref());
        let show_welcome = entries.is_empty() && streaming.is_none() && !sending;
        let has_older = active.is_some_and(|conversation| conversation.older_before.is_some());
        let blocks = collect_transcript_blocks(entries);
        let show_speakers = compute_block_speakers(&blocks, show_reasoning);
        let reduce_motion = cx.reduce_motion();
        let mut elements: Vec<gpui_kit::AnyElement> = Vec::with_capacity(blocks.len());
        let mut turn_has_assistant_header = false;
        for (item, show_speaker) in blocks.into_iter().zip(show_speakers) {
            match item {
                TranscriptBlock::User { entry, index } => {
                    turn_has_assistant_header = false;
                    let child = if mycode_app::is_compaction_summary(&entry.text) {
                        transcript::render_summary_entry(entry, cx.theme()).into_any_element()
                    } else {
                        let can_rewind = workspace.can_rewind_message(index);
                        transcript::render_user_entry(entry, can_rewind, index, cx)
                    };
                    elements.push(appear(entry.event_id.as_str(), reduce_motion, child));
                }
                TranscriptBlock::Tools(tools) => {
                    let id = tools
                        .first()
                        .map(|tool| tool.call.event_id.as_str())
                        .unwrap_or("tools");
                    elements.push(appear(
                        id,
                        reduce_motion,
                        render_tool_run(&tools, workspace, sending, reduce_motion, cx),
                    ));
                }
                TranscriptBlock::Entry(entry) => {
                    if show_speaker {
                        turn_has_assistant_header = true;
                    }
                    elements.push(appear(
                        &entry.event_id,
                        reduce_motion,
                        transcript::render_entry(
                            entry,
                            workspace,
                            Some(&md_cache),
                            cx.theme(),
                            transcript::AssistantDisplay {
                                reasoning: show_reasoning,
                                speaker: show_speaker,
                            },
                            window,
                            cx,
                        ),
                    ));
                }
            }
        }
        let streaming_show_speaker = !turn_has_assistant_header;
        let streaming_element = streaming
            .map(|streaming| {
                transcript::render_streaming_entry(
                    streaming,
                    workspace,
                    Some(&md_cache),
                    cx.theme(),
                    transcript::AssistantDisplay {
                        reasoning: show_reasoning,
                        speaker: streaming_show_speaker,
                    },
                    window,
                    cx,
                )
                .into_any_element()
            })
            .or_else(|| {
                sending.then(|| {
                    transcript::render_streaming_entry(
                        &crate::view_model::StreamingReply {
                            status: crate::i18n::t("Waiting for the model", "等待模型响应")
                                .to_owned(),
                            ..crate::view_model::StreamingReply::default()
                        },
                        workspace,
                        Some(&md_cache),
                        cx.theme(),
                        transcript::AssistantDisplay {
                            reasoning: show_reasoning,
                            speaker: streaming_show_speaker,
                        },
                        window,
                        cx,
                    )
                    .into_any_element()
                })
            });
        (show_welcome, has_older, elements, streaming_element)
    };
    let scroll_handle = workspace.conversation_scroll_handle().clone();
    let appearance = workspace
        .take_appearance_scroll()
        .map(|(pinned, offset_y)| scroll_hold::AppearanceHold { pinned, offset_y });
    let anchor = scroll_hold::ScrollAnchor::new(
        scroll_handle.clone(),
        workspace.take_scroll_anchor(),
        appearance,
    );
    let probe = anchor.probe();
    let theme = cx.theme();
    // Details docks into the row while pinned, so this pane is already the
    // strip between the sidebar and that column. When the card floats,
    // reserve its width or the transcript and composer slide underneath it.
    let details_docked = workspace.vm().inspector_open && workspace.vm().inspector_pinned;
    let chat_width = f32::from(window.viewport_size().width) - super::sidebar::WIDTH_PX;
    let details_reserve = super::context::chat_right_reserve(
        workspace.vm().inspector_open,
        details_docked,
        chat_width,
    );
    div()
        .id("chat")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(super::skin::ambient(theme))
        .when(details_reserve > 0., |this| this.pr(px(details_reserve)))
        .child(
            div()
                .id("conversation-frame")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    anchor.child(
                        div()
                            .id("conversation")
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_x_hidden()
                            .overflow_y_scroll()
                            .track_scroll(&scroll_handle)
                            .on_scroll_wheel(cx.listener(|workspace, _, _, cx| {
                                workspace.on_conversation_scrolled(cx);
                            }))
                            .on_mouse_up(
                                gpui_kit::MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.on_conversation_scrolled(cx);
                                }),
                            )
                            .flex()
                            .flex_col()
                            .child(
                                probe.child(
                                    transcript::transcript_stack()
                                        // Breathing room under the newest
                                        // row so the latest reply or
                                        // thinking trace does not sit flush
                                        // against the composer.
                                        .pb(px(40.))
                                        .when(show_welcome, |this| {
                                            this.child(welcome::render_welcome(workspace, cx))
                                        })
                                        .when(has_older || loading_older, |this| {
                                            this.child(render_older_chip(loading_older, cx))
                                        })
                                        .children(entry_elements)
                                        .when_some(streaming_element, |this, streaming| {
                                            this.child(streaming)
                                        })
                                        .when_some(
                                            workspace.vm().turn_alert.as_ref(),
                                            |this, alert| {
                                                this.child(render_turn_alert(alert, cx.theme()))
                                            },
                                        ),
                                ),
                            ),
                    ),
                ),
        )
        .when(
            workspace
                .vm()
                .mention
                .as_ref()
                .is_some_and(|mention| !mention.items.is_empty()),
            |this| this.child(menus::render_mention_layer(workspace, window, cx)),
        )
        // The jump-to-latest row rides the same docked pattern, right above
        // the composer and outside it, and shows only while the transcript
        // trails its tail.
        .when(
            !show_welcome && !workspace.conversation_follows_tail(),
            |this| this.child(render_jump_to_latest(cx)),
        )
        .child(composer::render_composer(workspace, window, cx))
        .into_any_element()
}

/// Red transcript-tail row for a turn that closed without a reply. The
/// same alert a failed tool row paints, so an interrupted or failed turn
/// is not mistaken for a completed answer.
fn render_turn_alert(
    message: &str,
    theme: &gpui_kit::component::theme::Theme,
) -> gpui_kit::AnyElement {
    div()
        .id("turn-alert")
        .w_full()
        .px_4()
        .py(px(6.))
        .flex()
        .items_center()
        .gap_2()
        .child(crate::ui::lamp(theme.danger))
        .child(
            div()
                .text_sm()
                .text_color(theme.danger)
                .child(message.to_owned()),
        )
        .into_any_element()
}

fn appear(id: &str, reduce_motion: bool, child: gpui_kit::AnyElement) -> gpui_kit::AnyElement {
    super::motion::fade_in(
        format!("enter-{id}"),
        reduce_motion,
        div().w_full().min_w_0().child(child),
    )
}

enum TranscriptBlock<'a> {
    User {
        entry: &'a ConversationEntry,
        index: usize,
    },
    Tools(Vec<ToolRow<'a>>),
    Entry(&'a ConversationEntry),
}

struct ToolRow<'a> {
    call: &'a ConversationEntry,
    result: Option<&'a ConversationEntry>,
}

/// Computes which transcript blocks should render an assistant speaker header
/// so only the first visible assistant block in each turn gets one.
fn compute_block_speakers(blocks: &[TranscriptBlock<'_>], show_reasoning: bool) -> Vec<bool> {
    let mut turn_has_header = false;
    blocks
        .iter()
        .map(|block| match block {
            TranscriptBlock::User { .. } => {
                turn_has_header = false;
                false
            }
            TranscriptBlock::Tools(_) => false,
            TranscriptBlock::Entry(entry) => {
                let renders_assistant = entry.kind == EntryKind::AssistantMessage
                    && (!entry.text.trim().is_empty()
                        || (show_reasoning && !entry.thinking.is_empty()));
                if renders_assistant && !turn_has_header {
                    turn_has_header = true;
                    true
                } else {
                    false
                }
            }
        })
        .collect()
}

fn render_tool_run(
    tools: &[ToolRow<'_>],
    workspace: &Workspace,
    turn_running: bool,
    reduce_motion: bool,
    cx: &gpui_kit::Context<Workspace>,
) -> gpui_kit::AnyElement {
    if tools.is_empty() {
        return div().into_any_element();
    }
    let theme = cx.theme();
    let rail = theme.border.opacity(0.65);
    if tools.len() >= 2 {
        let id = tools[0].call.event_id.clone();
        let labels: Vec<&str> = tools.iter().map(|tool| tool.call.text.as_ref()).collect();
        let open = workspace.tool_batch_open(&id);
        let has_failed = tools.iter().any(|tool| {
            tool.result
                .is_some_and(|entry| entry.text.starts_with("failed:"))
        });
        let has_unfinished = tools.iter().any(|tool| tool.result.is_none());
        // A running subagent's cancel `x` must stay reachable, so a run
        // holding one stays open while it lives.
        let open = open
            || tools
                .iter()
                .any(|tool| tool_cancellable(tool, workspace, turn_running));
        let mut column = div()
            .w_full()
            .min_w_0()
            .pl(super::skin::space(2))
            .border_l_1()
            .border_color(rail)
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(tools::render_tool_batch(
                &id,
                &labels,
                open,
                has_failed,
                has_unfinished,
                turn_running,
                cx,
            ));
        if open {
            let cards = tools
                .iter()
                .map(|tool| render_one_tool(tool, workspace, turn_running, reduce_motion, cx));
            column = column.child(super::motion::fade_in(
                format!("tool-run-{id}"),
                reduce_motion,
                div().w_full().min_w_0().flex().flex_col().children(cards),
            ));
        }
        return column.into_any_element();
    }
    div()
        .w_full()
        .min_w_0()
        .pl(super::skin::space(2))
        .border_l_1()
        .border_color(rail)
        .flex()
        .flex_col()
        .child(render_one_tool(
            &tools[0],
            workspace,
            turn_running,
            reduce_motion,
            cx,
        ))
        .into_any_element()
}

fn render_one_tool(
    tool: &ToolRow<'_>,
    workspace: &Workspace,
    turn_running: bool,
    reduce_motion: bool,
    cx: &gpui_kit::Context<Workspace>,
) -> gpui_kit::AnyElement {
    let expanded = workspace.tool_row_open(&tool.call.event_id);
    let cancellable = tool_cancellable(tool, workspace, turn_running);
    // The running clock lives on the job; once it leaves `live_jobs` its
    // final runtime was parked on the state.
    let elapsed_ms = tool.call.call_id.as_deref().and_then(|call_id| {
        workspace
            .vm()
            .live_jobs
            .iter()
            .find(|job| job.call_id == call_id)
            .map(crate::view_model::LiveJob::elapsed_ms)
            .or_else(|| workspace.vm().job_elapsed(call_id))
    });
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(tools::render_tool_block(
            tool.call,
            tool.result,
            turn_running,
            tools::ToolRowFlags {
                expanded,
                cancellable,
                reduce_motion,
                elapsed_ms,
            },
            cx,
        ))
        .into_any_element()
}

fn tool_cancellable(tool: &ToolRow<'_>, workspace: &Workspace, turn_running: bool) -> bool {
    crate::view_model::subagent_cancellable(
        tool.call.call_id.as_deref(),
        tool.result.is_some(),
        turn_running,
        &workspace.vm().live_jobs,
    )
}

/// Groups the transcript in linear time without cloning message payloads.
/// Results are indexed backwards so reused call ids still select the nearest
/// later result, never one from an earlier turn. The forward pass groups tools
/// directly, without a second intermediate item representation.
fn collect_transcript_blocks(entries: &[ConversationEntry]) -> Vec<TranscriptBlock<'_>> {
    let mut next_results = std::collections::HashMap::new();
    let mut matched = vec![None; entries.len()];
    for (index, entry) in entries.iter().enumerate().rev() {
        if let Some(call) = entry.call_id.as_deref() {
            match entry.kind {
                EntryKind::ToolResult => {
                    next_results.insert(call, entry);
                }
                EntryKind::ToolCall => matched[index] = next_results.get(call).copied(),
                _ => {}
            }
        }
    }

    let mut blocks = Vec::with_capacity(entries.len());
    let mut run = Vec::new();
    let mut listed_calls = std::collections::HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.kind == EntryKind::ToolCall {
            if let Some(call) = entry.call_id.as_deref() {
                listed_calls.insert(call);
            }
            run.push(ToolRow {
                call: entry,
                result: matched[index],
            });
            continue;
        }
        if (entry.kind == EntryKind::AssistantMessage
            && entry.text.trim().is_empty()
            && entry.thinking.trim().is_empty())
            || (entry.kind == EntryKind::ToolResult
                && entry
                    .call_id
                    .as_deref()
                    .is_some_and(|call| listed_calls.contains(call)))
        {
            continue;
        }
        if !run.is_empty() {
            blocks.push(TranscriptBlock::Tools(std::mem::take(&mut run)));
        }
        blocks.push(if entry.kind == EntryKind::UserMessage {
            TranscriptBlock::User { entry, index }
        } else {
            TranscriptBlock::Entry(entry)
        });
    }
    if !run.is_empty() {
        blocks.push(TranscriptBlock::Tools(run));
    }
    blocks
}

/// The jump-to-latest control, docked in-flow right above the composer. The
/// click returns the transcript to its tail; the row unmounts on the next
/// frame because the tail is followed again.
fn render_jump_to_latest(cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("conversation-jump-row")
        .w_full()
        .px_4()
        .pb(px(menus::JUMP_LATEST_ROW_PAD))
        .flex()
        .justify_center()
        .child(
            div()
                .id("conversation-jump-latest")
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .size(px(menus::JUMP_LATEST_BUTTON_PX))
                .rounded_full()
                .border_1()
                .border_color(super::skin::glass_border(theme))
                .bg(super::skin::popover(theme))
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|this| {
                    this.bg(super::skin::frost_hover(theme))
                        .text_color(theme.foreground)
                })
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.scroll_conversation_to_bottom(cx);
                }))
                .child(
                    gpui_kit::component::Icon::new(gpui_kit::assets::IconName::ArrowDown).small(),
                ),
        )
}

/// The control above a tail window. The count of remaining events is not
/// known without reading them, so the label stays generic.
fn render_older_chip(loading: bool, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    let label = if loading {
        t("Loading earlier messages…", "正在加载更早的消息…")
    } else {
        t("Earlier messages", "更早的消息")
    };
    div()
        .id("transcript-older")
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_2()
        .py(px(8.))
        .rounded(px(10.))
        .when(!loading, |this| {
            this.cursor_pointer()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_load_older(cx);
                }))
        })
        .child(crate::ui::lamp(desk.violet))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
}

#[cfg(test)]
mod nesting_tests {
    use super::{TranscriptBlock, collect_transcript_blocks};
    use crate::view_model::{ConversationEntry, EntryKind};

    fn call(id: &str, text: &str) -> ConversationEntry {
        ConversationEntry {
            event_id: format!("call-{id}"),
            kind: EntryKind::ToolCall,
            text: text.into(),
            call_id: Some(id.to_owned()),
            thinking: String::new(),
        }
    }

    fn result(id: &str, text: &str) -> ConversationEntry {
        ConversationEntry {
            event_id: format!("result-{id}"),
            kind: EntryKind::ToolResult,
            text: text.into(),
            call_id: Some(id.to_owned()),
            thinking: String::new(),
        }
    }

    #[test]
    fn reused_call_ids_pair_with_the_nearest_later_result() {
        let entries = vec![
            result("a", "orphan"),
            call("a", "first"),
            call("b", "parallel"),
            result("b", "parallel result"),
            result("a", "first result"),
            call("a", "second"),
            result("a", "second result"),
            call("a", "unfinished"),
        ];
        let blocks = collect_transcript_blocks(&entries);
        assert_eq!(blocks.len(), 2);
        assert!(
            matches!(&blocks[0], TranscriptBlock::Entry(entry) if entry.text.as_ref() == "orphan")
        );
        let TranscriptBlock::Tools(tools) = &blocks[1] else {
            panic!("expected a tool run");
        };
        let results: Vec<_> = tools
            .iter()
            .map(|tool| tool.result.map(|entry| entry.text.as_ref()))
            .collect();
        assert_eq!(
            results,
            [
                Some("first result"),
                Some("parallel result"),
                Some("second result"),
                None
            ]
        );
    }

    #[test]
    fn a_large_parallel_run_keeps_every_result_with_its_call() {
        let mut entries = Vec::new();
        for index in 0..1024 {
            entries.push(call(&index.to_string(), "read"));
        }
        for index in (0..1024).rev() {
            entries.push(result(&index.to_string(), &format!("payload {index}")));
        }
        let blocks = collect_transcript_blocks(&entries);
        assert_eq!(blocks.len(), 1);
        let TranscriptBlock::Tools(tools) = &blocks[0] else {
            panic!("expected a tool run");
        };
        assert_eq!(tools.len(), 1024);
        for (index, tool) in tools.iter().enumerate() {
            assert_eq!(
                tool.result.expect("paired result").text.as_ref(),
                format!("payload {index}")
            );
        }
    }

    #[test]
    fn consecutive_tool_rows_collapse_into_one_run() {
        let user = crate::view_model::ConversationEntry {
            event_id: "user".into(),
            kind: EntryKind::UserMessage,
            text: "hi".into(),
            call_id: None,
            thinking: String::new(),
        };
        let agent = crate::view_model::ConversationEntry {
            event_id: "agent".into(),
            kind: EntryKind::AssistantMessage,
            text: "done".into(),
            call_id: None,
            thinking: String::new(),
        };
        let entries = vec![
            user,
            call("a", "read  a.rs"),
            result("a", "alpha"),
            call("b", "edit  b.rs"),
            result("b", "+ x\n"),
            agent,
            call("c", "read  c.rs"),
            result("c", "c"),
        ];
        let blocks = collect_transcript_blocks(&entries);
        assert!(matches!(
            blocks.first(),
            Some(super::TranscriptBlock::User { .. })
        ));
        let super::TranscriptBlock::Tools(first) = &blocks[1] else {
            panic!("expected the two tool calls to share one run");
        };
        assert_eq!(first.len(), 2);
        assert!(matches!(
            blocks.get(2),
            Some(super::TranscriptBlock::Entry(_))
        ));
        let super::TranscriptBlock::Tools(second) = &blocks[3] else {
            panic!("a reply should start a new tool run");
        };
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn only_the_first_assistant_step_per_turn_receives_a_speaker_header() {
        let user = |id: &str| ConversationEntry {
            event_id: id.into(),
            kind: EntryKind::UserMessage,
            text: "prompt".into(),
            call_id: None,
            thinking: String::new(),
        };
        let assistant = |id: &str, text: &str, thinking: &str| ConversationEntry {
            event_id: id.into(),
            kind: EntryKind::AssistantMessage,
            text: text.into(),
            call_id: None,
            thinking: thinking.into(),
        };
        let entries = vec![
            user("u1"),
            assistant("a1", "first step", ""),
            call("c1", "read  a.rs"),
            result("c1", "ok"),
            assistant("a2", "second step", "some trace"),
            user("u2"),
            assistant("a3", "", "thinking only"),
            assistant("a4", "final answer", ""),
        ];
        let blocks = collect_transcript_blocks(&entries);
        let speakers = super::compute_block_speakers(&blocks, true);
        // Blocks: u1 (false), a1 (true), tools (false), a2 (false), u2 (false), a3 (true), a4 (false)
        assert_eq!(
            speakers,
            vec![false, true, false, false, false, true, false]
        );
        // When reasoning is hidden ("off"), a3 has no visible content so a4 gets the header.
        let speakers_no_reasoning = super::compute_block_speakers(&blocks, false);
        assert_eq!(
            speakers_no_reasoning,
            vec![false, true, false, false, false, false, true]
        );
    }
}
