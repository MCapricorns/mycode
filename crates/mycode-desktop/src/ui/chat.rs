//! The chat column: centered conversation transcript, the pending-ask panel,
//! and the integrated composer carrying the project and model chips.
mod ask;
mod composer;
mod menus;
mod scroll_hold;
mod transcript;
mod welcome;

pub(crate) use composer::composer_placeholder;
pub(crate) use menus::reasoning_row_label;
pub(crate) use welcome::new_task_label;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use crate::i18n::t;
use crate::view_model::{ConversationEntry, EntryKind};
use crate::workspace::Workspace;

/// Conversation column. Side whitespace keeps the transcript in a readable measure.
pub(super) const COLUMN_MAX: gpui_kit::Pixels = px(760.);

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
        let items = collect_transcript_items(entries);
        let mut elements: Vec<gpui_kit::AnyElement> = Vec::with_capacity(items.len());
        for item in items {
            match item {
                TranscriptItem::User { entry, index } => {
                    if mycode_app::is_compaction_summary(&entry.text) {
                        elements.push(
                            transcript::render_summary_entry(entry, cx.theme()).into_any_element(),
                        );
                    } else {
                        elements.push(transcript::render_user_entry(entry, index > 0, index, cx));
                    }
                }
                TranscriptItem::Tool { call, result } => {
                    let expanded = workspace.tool_row_open(&call.event_id);
                    elements.push(transcript::render_tool_block(call, result, expanded, cx));
                }
                TranscriptItem::Entry(entry) => {
                    elements.push(transcript::render_entry(
                        entry,
                        workspace,
                        cx.theme(),
                        show_reasoning,
                        cx,
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
    let anchor =
        scroll_hold::ScrollAnchor::new(scroll_handle.clone(), workspace.take_scroll_anchor());
    let probe = anchor.probe();
    let theme = cx.theme();
    div()
        .id("chat")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(super::skin::ambient(theme))
        .child(
            div()
                .id("conversation-frame")
                .relative()
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
                )
                .child(render_conversation_jumps(cx)),
        )
        // The model menu docks in-flow right above the composer: an
        // absolutely positioned overlay landed outside the visible window on
        // mis-scaled displays, and a docked panel cannot be clipped away.
        .when(workspace.vm().model_menu_open, |this| {
            this.child(menus::render_model_menu(workspace, window, cx))
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

/// Jump controls pinned to the bottom-right of the scroll region, above the
/// composer. They sit outside the scroller so they do not travel with the
/// transcript. `stop_propagation` keeps the click from starting a scroll drag.
fn render_conversation_jumps(cx: &Context<Workspace>) -> impl IntoElement {
    div()
        .id("conversation-jumps")
        .absolute()
        .bottom(px(8.))
        .right(px(12.))
        .flex()
        .flex_row()
        .gap_1()
        .child(conversation_jump(
            "conversation-jump-top",
            t("Top", "顶部"),
            true,
            cx,
        ))
        .child(conversation_jump(
            "conversation-jump-bottom",
            t("Bottom", "底部"),
            false,
            cx,
        ))
}

fn conversation_jump(
    id: &'static str,
    label: &'static str,
    to_top: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(id)
        .occlude()
        .flex()
        .items_center()
        .h(px(22.))
        .px_2()
        .rounded(super::skin::radius_control())
        .border_1()
        .border_color(super::skin::glass_border(theme))
        .bg(super::skin::popover(theme))
        .text_xs()
        .text_color(theme.muted_foreground)
        .cursor_pointer()
        .hover(|this| this.bg(super::skin::frost_hover(theme)))
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .on_click(cx.listener(move |workspace, _, _, cx| {
            cx.stop_propagation();
            if to_top {
                workspace.scroll_conversation_to_top(cx);
            } else {
                workspace.scroll_conversation_to_bottom(cx);
            }
        }))
        .child(label)
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
