//! The transcript timeline: streaming bubble, committed entries, tool
//! blocks, and user rows with their edit/recall hover actions.
use gpui_kit::assets::IconName;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, rems,
};

use crate::i18n::t;
use crate::ui::skin::{self, mono_chip};
use crate::ui::{desk::Desk, ellipsis};
use crate::view_model::{ConversationEntry, EntryKind, StreamingReply};
use crate::workspace::Workspace;

/// The in-flight assistant turn: a live status line, then thinking and text.
///
/// Subagent progress lives on this one status line and in the inspector
/// panel; the transcript itself stays free of per-child rows.
pub(super) fn render_streaming_entry(
    streaming: &StreamingReply,
    theme: &Theme,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let _ = cx;
    let desk = Desk::of(theme);
    let status = if streaming.status.is_empty() {
        t("Working", "处理中").to_owned()
    } else {
        streaming.status.clone()
    };
    desk_shell(
        "live".to_owned(),
        theme,
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(mono_chip(
                &format!("{} · {status}", t("WORKING", "进行中")),
                desk.amber,
                desk.amber.opacity(0.45),
                theme,
            ))
            .when(!streaming.thinking.is_empty(), |this| {
                this.child(thinking_box(
                    "streaming-thinking".into(),
                    &streaming.thinking,
                    theme,
                ))
            })
            .when(!streaming.text.trim().is_empty(), |this| {
                this.child(mono_chip(
                    "AGENT",
                    desk.green,
                    desk.green.opacity(0.35),
                    theme,
                ))
                .child(agent_text(
                    "streaming-agent-md".into(),
                    streaming.text.clone().into(),
                    theme,
                ))
            })
            .when(
                streaming.thinking.trim().is_empty() && streaming.text.trim().is_empty(),
                |this| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                },
            ),
    )
}

/// Desk transcript entries: the demo's timeline blocks — a mono stamp gutter
/// plus a ledger row per entry. Assistant text is bare (`.msg-agent`); user
/// rows and tool blocks arrive pre-routed by the caller's transcript
/// collection and render through their own builders.
pub(super) fn render_entry(entry: &ConversationEntry, theme: &Theme) -> gpui_kit::AnyElement {
    match entry.kind {
        EntryKind::AssistantMessage => desk_block(entry, theme, {
            let desk = Desk::of(theme);
            div()
                .flex()
                .flex_col()
                .gap_2()
                .when(!entry.thinking.is_empty(), |this| {
                    this.child(mono_chip("THINKING", desk.faint, theme.border, theme))
                        .child(thinking_box(
                            format!("thinking-{}", entry.event_id).into(),
                            &entry.thinking,
                            theme,
                        ))
                })
                .when(!entry.text.trim().is_empty(), |this| {
                    this.child(mono_chip(
                        "AGENT",
                        desk.green,
                        desk.green.opacity(0.35),
                        theme,
                    ))
                    .child(agent_text(
                        format!("agent-md-{}", entry.event_id).into(),
                        SharedString::from(entry.text.trim()),
                        theme,
                    ))
                })
        })
        .into_any_element(),
        EntryKind::ToolResult => {
            let desk = Desk::of(theme);
            let failed = entry.text.starts_with("failed:");
            desk_block(
                entry,
                theme,
                div()
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
        EntryKind::Usage => div()
            .id(format!("entry-{}", entry.event_id))
            .into_any_element(),
        // Unreachable through the only caller: the transcript collection
        // routes user rows and tool calls to their own builders before any
        // entry reaches this match. The arm only keeps the match total.
        EntryKind::UserMessage | EntryKind::ToolCall => div().into_any_element(),
    }
}

/// The timeline block shell: stamp gutter + content, matching the demo's
/// `.block` (left stamp, hover anchor omitted — no interaction change).
fn desk_block(
    entry: &ConversationEntry,
    theme: &Theme,
    content: impl IntoElement,
) -> impl IntoElement {
    desk_shell(
        short_stamp(&entry.event_id),
        theme,
        div()
            .id(format!("entry-{}", entry.event_id))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(content),
    )
}

fn desk_shell(stamp: String, theme: &Theme, content: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .w_full()
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .pt(px(3.))
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(Desk::of(theme).faint)
                .whitespace_nowrap()
                .child(stamp),
        )
        .child(content)
}

fn thinking_box(id: SharedString, text: &str, theme: &Theme) -> impl IntoElement {
    let desk = Desk::of(theme);
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .border_1()
        .border_color(skin::glass_border(theme))
        .rounded(skin::radius_card())
        .px_3()
        .py_2()
        .bg(skin::frost(theme))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(desk.faint)
                .child(div().text_color(desk.amber).child("▸"))
                .child("REASONING"),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(text.to_owned()),
        )
}

/// Assistant reply bubble: Markdown via gpui-kit's TextView.
///
/// Fenced code is highlighted by the kit's tree-sitter grammars
/// (`tree-sitter-languages`). The highlighter is installed with the theme
/// (`install_text_view_defaults` on `Theme::change` and `Theme::sync_base`).
/// The id must be unique per entry — `ElementId::CodeLocation` would collide
/// across blocks since all bubbles render from the same call site.
fn agent_text(id: SharedString, text: SharedString, theme: &Theme) -> impl IntoElement {
    // The theme default leaves a full rem between paragraphs, which paints
    // as a tall empty slab when a reply is short or still streaming.
    let style = TextViewStyle::default().paragraph_gap(rems(0.35));
    div()
        .w_full()
        .min_w_0()
        .rounded(skin::radius_card())
        .px_3()
        .py_2()
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::frost(theme))
        .child(TextView::markdown(id, text).style(style).selectable(true))
}

/// Stable short stamp for the gutter: the entry id is a ledger identity, not
/// a clock time, so show its tail (mirrors `short_id`, 8 chars, mono).
fn short_stamp(event_id: &str) -> String {
    let tail: String = event_id
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    tail
}

/// One tool ledger block: header row (lamp + name + target) with the result
/// rendered inside as diff-green/red or CRT terminal text. Failures paint the
/// border red; a pending call (no result yet) shows the amber lamp.
pub(super) fn render_tool_block(
    call: &ConversationEntry,
    result: Option<&ConversationEntry>,
    theme: &Theme,
) -> gpui_kit::AnyElement {
    let desk = Desk::of(theme);
    let failed = result.is_some_and(|r| r.text.starts_with("failed:"));
    let waiting = result.is_none();
    let lamp_color = if failed {
        desk.red
    } else if waiting {
        desk.amber
    } else {
        desk.green
    };
    let mut card = div()
        .flex()
        .flex_col()
        .rounded(skin::radius_card())
        .border_1()
        .border_color(if failed {
            desk.red.opacity(0.45)
        } else {
            skin::glass_border(theme)
        })
        .bg(skin::frost(theme))
        .w_full()
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(7.))
                .text_xs()
                .child(crate::ui::lamp(lamp_color))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(call.text.to_string()),
                ),
        );
    if let Some(result) = result {
        let body = result_body(tool_name(call.text.as_ref()), result, theme, &desk);
        card = card.child(
            div()
                .border_t_1()
                .border_color(skin::glass_border(theme))
                .child(body),
        );
    }
    desk_block(call, theme, card).into_any_element()
}

/// The first token of a tool label is the tool name. The rest is the target.
fn tool_name(label: &str) -> &str {
    label.split_whitespace().next().unwrap_or(label)
}

/// The tool result body: edit diffs and search hits render as a preview
/// so additions, deletions, and matches stay visible. Other long output
/// stays a dark CRT strip.
fn result_body(
    tool: &str,
    result: &ConversationEntry,
    theme: &Theme,
    desk: &Desk,
) -> impl IntoElement {
    let text = result.text.to_string();
    let failed = text.starts_with("failed:");
    let lines: Vec<&str> = text.lines().collect();
    let diff_lines = lines
        .iter()
        .filter(|line| {
            line.starts_with("+ ")
                || line.starts_with("- ")
                || line.strip_prefix("[diff truncated]").is_some()
        })
        .count();
    if diff_lines > 0 {
        diff_preview(&lines, theme, desk).into_any_element()
    } else if matches!(tool, "grep" | "find" | "search") && !failed {
        search_preview(tool, &lines, theme, desk).into_any_element()
    } else if failed || text.len() > 300 || lines.len() > 6 {
        div()
            .w_full()
            .min_w_0()
            .px_2()
            .py_1()
            .text_xs()
            .whitespace_normal()
            .font_family(theme.mono_font_family.clone())
            .bg(desk.screen.opacity(0.45))
            .text_color(if failed { desk.red } else { desk.screen_dim })
            .child(ellipsis(&text, 2000))
            .into_any_element()
    } else {
        div()
            .px_2()
            .py_1()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .child(ellipsis(&text, 600))
            .into_any_element()
    }
}

fn diff_preview(lines: &[&str], theme: &Theme, desk: &Desk) -> impl IntoElement {
    let added = lines.iter().filter(|line| line.starts_with("+ ")).count();
    let removed = lines.iter().filter(|line| line.starts_with("- ")).count();
    let shown = lines.len().min(80);
    div()
        .flex()
        .flex_col()
        .py_1()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .child(preview_caption(
            &format!("DIFF  +{added}  -{removed}"),
            desk.green,
            theme,
        ))
        .children(lines.iter().take(shown).map(|line| {
            let (color, bg) = if line.starts_with("+ ") {
                (desk.green, desk.green.opacity(0.08))
            } else if line.starts_with("- ") {
                (desk.red, desk.red.opacity(0.08))
            } else {
                (theme.muted_foreground, theme.transparent)
            };
            div()
                .px_2()
                .text_color(color)
                .bg(bg)
                .child((*line).to_owned())
        }))
        .when(lines.len() > shown, |this| {
            this.child(preview_caption(
                t("… more changes omitted", "… 更多改动已省略"),
                desk.faint,
                theme,
            ))
        })
}

fn search_preview(tool: &str, lines: &[&str], theme: &Theme, desk: &Desk) -> impl IntoElement {
    let hits = lines.iter().filter(|line| !line.starts_with('[')).count();
    let label = if tool == "find" { "PATHS" } else { "MATCHES" };
    let shown = lines.len().min(40);
    div()
        .flex()
        .flex_col()
        .py_1()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .child(preview_caption(
            &format!("{label}  {hits}"),
            desk.amber,
            theme,
        ))
        .children(lines.iter().take(shown).map(|line| {
            let notice = line.starts_with('[');
            div()
                .px_2()
                .text_color(if notice {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .bg(if notice {
                    theme.transparent
                } else {
                    desk.green.opacity(0.06)
                })
                .child((*line).to_owned())
        }))
        .when(lines.len() > shown, |this| {
            this.child(preview_caption(
                t("… more results omitted", "… 更多结果已省略"),
                desk.faint,
                theme,
            ))
        })
}

fn preview_caption(label: &str, color: gpui_kit::Hsla, theme: &Theme) -> impl IntoElement {
    div()
        .px_2()
        .py(px(2.))
        .text_color(color)
        .child(label.to_owned())
        .font_family(theme.mono_font_family.clone())
}

/// One user bubble with hover actions: edit-and-resend (rewinds to before
/// this message and prefills the composer) and recall (drops this message
/// and everything after). Neither action restores or deletes workspace
/// files. The first message has no prior event to rewind to, so its
/// actions hide.
pub(super) fn render_user_entry(
    entry: &ConversationEntry,
    can_rewind: bool,
    index: usize,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let desk = Desk::of(theme);
    // Desk `.msg-user`: plain left-aligned ledger row with a cyan ▸ arrow.
    // The hover edit/recall actions are unchanged — they now sit inline to
    // the right of the text instead of under a right-aligned bubble.
    let mut column = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(mono_chip("YOU", desk.cyan, desk.cyan.opacity(0.35), theme))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .w_full()
                .min_w_0()
                .rounded(skin::radius_card())
                .border_1()
                .border_color(skin::glass_border(theme))
                .bg(skin::frost(theme))
                .px_3()
                .py_2()
                .text_sm()
                .child(div().text_color(desk.cyan).child("▸"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .child(SharedString::from(&entry.text)),
                ),
        );
    if can_rewind {
        column = column.child(
            div()
                .id(format!("entry-actions-{index}"))
                .flex()
                .flex_row()
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
                ),
        );
    }
    div()
        .id(format!("entry-{}", entry.event_id))
        .group("user-entry")
        .child(desk_shell(short_stamp(&entry.event_id), theme, column))
        .into_any_element()
}
