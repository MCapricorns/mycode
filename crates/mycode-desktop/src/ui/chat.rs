//! The chat column: centered conversation transcript, the pending-ask panel,
//! and the integrated composer carrying the project and model chips.
mod ask;
mod composer;
mod menus;
mod transcript;
mod welcome;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use crate::ui::skin;
use crate::view_model::{ConversationEntry, EntryKind, transcript_start};
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
    let extra = workspace.vm().transcript_extra;
    let (show_welcome, hidden, entry_elements, streaming_element) = {
        let active = workspace.vm().active.as_ref();
        let entries: &[ConversationEntry] = active
            .map(|conversation| conversation.entries.as_slice())
            .unwrap_or_default();
        let streaming = active.and_then(|c| c.streaming.as_ref());
        let show_welcome = entries.is_empty() && streaming.is_none() && !sending;
        let items = collect_transcript_items(entries);
        let start = transcript_start(items.len(), extra);
        let hidden = start;
        let mut elements: Vec<gpui_kit::AnyElement> = Vec::with_capacity(items.len() - start);
        for item in items.into_iter().skip(start) {
            match item {
                TranscriptItem::User { entry, index } => {
                    elements.push(transcript::render_user_entry(entry, index > 0, index, cx));
                }
                TranscriptItem::Tool { call, result } => {
                    elements.push(transcript::render_tool_block(call, result, cx.theme()));
                }
                TranscriptItem::Entry(entry) => {
                    elements.push(transcript::render_entry(entry, cx.theme()));
                }
            }
        }
        let streaming_element = streaming
            .map(|streaming| {
                transcript::render_streaming_entry(streaming, cx.theme(), cx).into_any_element()
            })
            .or_else(|| {
                sending.then(|| {
                    transcript::render_streaming_entry(
                        &crate::view_model::StreamingReply {
                            status: crate::i18n::t("Waiting for the model", "等待模型响应")
                                .to_owned(),
                            ..crate::view_model::StreamingReply::default()
                        },
                        cx.theme(),
                        cx,
                    )
                    .into_any_element()
                })
            });
        (show_welcome, hidden, elements, streaming_element)
    };
    let scroll_handle = workspace.conversation_scroll_handle().clone();
    div()
        .id("chat")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(
            div()
                .id("conversation")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .flex()
                .flex_col()
                .child(
                    div()
                        .id("conversation-inner")
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .py_4()
                        .px_4()
                        .when(show_welcome, |this| {
                            this.child(welcome::render_welcome(workspace, cx))
                        })
                        .when(hidden > 0, |this| this.child(render_fold_chip(hidden, cx)))
                        .children(entry_elements)
                        .when_some(streaming_element, |this, streaming| this.child(streaming)),
                ),
        )
        // The model menu docks in-flow right above the composer: an
        // absolutely positioned overlay landed outside the visible window on
        // mis-scaled displays, and a docked panel cannot be clipped away.
        .when(workspace.vm().model_menu_open, |this| {
            this.child(menus::render_model_menu(workspace, cx))
        })
        .when(workspace.vm().reasoning_menu_open, |this| {
            this.child(menus::render_thinking_menu(workspace, cx))
        })
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
        .child(composer::render_composer(workspace, window, cx))
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
    let mut index = 0;
    while index < entries.len() {
        let entry = &entries[index];
        if entry.kind == EntryKind::UserMessage {
            items.push(TranscriptItem::User { entry, index });
        } else if entry.kind == EntryKind::ToolCall {
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
            && entry.call_id.is_some()
            && entries[..index]
                .iter()
                .any(|prev| prev.kind == EntryKind::ToolCall && prev.call_id == entry.call_id)
        {
            // Already shown inside its call's block above.
        } else {
            items.push(TranscriptItem::Entry(entry));
        }
        index += 1;
    }
    items
}

/// The reveal control above a folded transcript. `hidden` counts folded
/// display blocks — user rows, tool blocks, and bare entries — not chat
/// messages, so it is spelled "entries".
fn render_fold_chip(hidden: usize, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    div()
        .id("transcript-fold")
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_2()
        .py(px(8.))
        .rounded(px(12.))
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::glass(theme))
        .cursor_pointer()
        .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.on_reveal_transcript(cx);
        }))
        .child(crate::ui::lamp(desk.violet))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{} {hidden}",
                    crate::i18n::t("earlier entries", "条更早的记录")
                )),
        )
}
