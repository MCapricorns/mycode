//! The chat column: conversation transcript, the pending-ask panel, and the
//! integrated composer. Both fill the strip between the sidebar and Details.
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
    // Off means the user asked not to see a reasoning trace. The blocks stay
    // in the ledger; only the REASONING row is hidden while that pick is set.
    let show_reasoning = crate::view_model::selected_reasoning_level(workspace.vm()) != "off";
    let (show_welcome, has_older, entry_elements, streaming_element) = {
        let active = workspace.vm().active.as_ref();
        let entries: &[ConversationEntry] = active
            .map(|conversation| conversation.entries.as_slice())
            .unwrap_or_default();
        let streaming = active.and_then(|c| c.streaming.as_ref());
        let show_welcome = entries.is_empty() && streaming.is_none() && !sending;
        let has_older = active.is_some_and(|conversation| conversation.older_before.is_some());
        let items = coalesce_tools(collect_transcript_items(entries));
        let reduce_motion = cx.reduce_motion();
        let mut elements: Vec<gpui_kit::AnyElement> = Vec::with_capacity(items.len());
        for item in items {
            match item {
                TranscriptBlock::User { entry, index } => {
                    let child = if mycode_app::is_compaction_summary(&entry.text) {
                        transcript::render_summary_entry(entry, cx.theme()).into_any_element()
                    } else {
                        transcript::render_user_entry(entry, index > 0, index, cx)
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
                        render_tool_run(&tools, workspace, reduce_motion, cx),
                    ));
                }
                TranscriptBlock::Entry(entry) => {
                    let id = entry.event_id.clone();
                    elements.push(appear(
                        &id,
                        reduce_motion,
                        transcript::render_entry(entry, workspace, cx.theme(), show_reasoning, cx),
                    ));
                }
            }
        }
        let streaming_element = streaming
            .map(|streaming| {
                transcript::render_streaming_entry(
                    streaming,
                    workspace,
                    cx.theme(),
                    show_reasoning,
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
                        cx.theme(),
                        show_reasoning,
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
    // Details docks into the row on a wide window, so this pane is already
    // the strip between the sidebar and that column. When the card floats,
    // reserve its width or the transcript and composer slide underneath it.
    let details_docked = super::DeskLayout::docks_inspector(window)
        && workspace.vm().inspector_open
        && workspace.vm().inspector_pinned;
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
                                        .when(show_welcome, |this| {
                                            this.child(welcome::render_welcome(workspace, cx))
                                        })
                                        .when(has_older || loading_older, |this| {
                                            this.child(render_older_chip(loading_older, cx))
                                        })
                                        .children(entry_elements)
                                        .when_some(streaming_element, |this, streaming| {
                                            this.child(streaming)
                                        }),
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
            |this| this.child(menus::render_mention_layer(workspace, cx)),
        )
        .when(workspace.vm().pending_ask.is_some(), |this| {
            this.child(ask::render_ask_panel(workspace, window, cx))
        })
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

fn coalesce_tools<'a>(items: Vec<TranscriptItem<'a>>) -> Vec<TranscriptBlock<'a>> {
    let mut blocks = Vec::with_capacity(items.len());
    let mut run: Vec<ToolRow<'a>> = Vec::new();
    for item in items {
        match item {
            TranscriptItem::User { entry, index } => {
                if !run.is_empty() {
                    blocks.push(TranscriptBlock::Tools(std::mem::take(&mut run)));
                }
                blocks.push(TranscriptBlock::User { entry, index });
            }
            TranscriptItem::Tool { call, result } => run.push(ToolRow { call, result }),
            TranscriptItem::Entry(entry) => {
                if !run.is_empty() {
                    blocks.push(TranscriptBlock::Tools(std::mem::take(&mut run)));
                }
                blocks.push(TranscriptBlock::Entry(entry));
            }
        }
    }
    if !run.is_empty() {
        blocks.push(TranscriptBlock::Tools(run));
    }
    blocks
}

fn render_tool_run(
    tools: &[ToolRow<'_>],
    workspace: &Workspace,
    reduce_motion: bool,
    cx: &gpui_kit::Context<Workspace>,
) -> gpui_kit::AnyElement {
    if tools.is_empty() {
        return div().into_any_element();
    }
    if tools.len() >= 2 {
        let id = tools[0].call.event_id.clone();
        let labels: Vec<&str> = tools.iter().map(|tool| tool.call.text.as_ref()).collect();
        let open = workspace.tool_batch_open(&id);
        let mut column = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(tools::render_tool_batch(
                &id,
                tools::tool_batch_summary(&labels),
                open,
                cx,
            ));
        if open {
            let cards = tools
                .iter()
                .map(|tool| render_one_tool(tool, workspace, reduce_motion, cx));
            column = column.child(super::motion::fade_in(
                format!("tool-run-{id}"),
                reduce_motion,
                div().w_full().min_w_0().flex().flex_col().children(cards),
            ));
        }
        return column.into_any_element();
    }
    render_one_tool(&tools[0], workspace, reduce_motion, cx)
}

fn render_one_tool(
    tool: &ToolRow<'_>,
    workspace: &Workspace,
    reduce_motion: bool,
    cx: &gpui_kit::Context<Workspace>,
) -> gpui_kit::AnyElement {
    let expanded = workspace.tool_row_open(&tool.call.event_id);
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(tools::render_tool_block(
            tool.call,
            tool.result,
            expanded,
            reduce_motion,
            cx,
        ))
        .into_any_element()
}

enum TranscriptItem<'a> {
    User {
        entry: &'a ConversationEntry,
        index: usize,
    },
    Tool {
        call: &'a ConversationEntry,
        result: Option<&'a ConversationEntry>,
    },
    Entry(&'a ConversationEntry),
}

/// Splits the raw entry list into display blocks: user messages become their
/// own rows, each tool call merges with its matching result, and everything
/// else (assistant text, orphan results, usage markers) passes through.
fn collect_transcript_items(entries: &[ConversationEntry]) -> Vec<TranscriptItem<'_>> {
    let mut items = Vec::with_capacity(entries.len());
    // Call ids whose tool call is already listed. Membership replaces a scan
    // back over every earlier entry, which went quadratic on long tool runs.
    let mut listed_calls: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut index = 0;
    while index < entries.len() {
        let entry = &entries[index];
        if entry.kind == EntryKind::UserMessage {
            items.push(TranscriptItem::User { entry, index });
        } else if entry.kind == EntryKind::ToolCall {
            if let Some(call) = entry.call_id.as_deref() {
                listed_calls.insert(call);
            }
            let result = entry.call_id.as_deref().and_then(|call| {
                entries[index + 1..].iter().find(|next| {
                    next.kind == EntryKind::ToolResult && next.call_id.as_deref() == Some(call)
                })
            });
            items.push(TranscriptItem::Tool {
                call: entry,
                result,
            });
        } else if entry.kind == EntryKind::AssistantMessage
            && entry.text.trim().is_empty()
            && entry.thinking.trim().is_empty()
        {
            // A step that only issued tool calls. The tool rows carry it.
        } else if entry.kind == EntryKind::ToolResult
            && entry
                .call_id
                .as_deref()
                .is_some_and(|call| listed_calls.contains(call))
        {
            // Already shown inside its call's block above.
        } else {
            items.push(TranscriptItem::Entry(entry));
        }
        index += 1;
    }
    items
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
    use super::collect_transcript_items;
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
        let blocks = super::coalesce_tools(collect_transcript_items(&entries));
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
}
