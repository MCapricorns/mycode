//! The transcript timeline: streaming bubble, committed entries, tool
//! blocks, and user rows with their edit/recall hover actions.
use gpui_kit::assets::IconName;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px, rems,
};

use crate::i18n::t;
use crate::ui::skin;
use crate::ui::{desk::Desk, ellipsis};
use crate::view_model::{ConversationEntry, EntryKind, StreamingReply};
use crate::workspace::Workspace;

/// The in-flight assistant turn: a live status line, then thinking and text.
///
/// Subagent progress lives on this one status line and in the inspector
/// panel; the transcript itself stays free of per-child rows.
pub(super) fn render_streaming_entry(
    streaming: &StreamingReply,
    workspace: &Workspace,
    theme: &Theme,
    show_reasoning: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let desk = Desk::of(theme);
    let status = if streaming.status.is_empty() {
        t("Working", "工作中").to_owned()
    } else {
        streaming.status.clone()
    };
    desk_shell(
        t("live", "实时").to_owned(),
        theme,
        div()
            .w_full()
            .min_w_0()
            .h_auto()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .child(status_line(
                "streaming-status",
                &format!("{} · {status}", t("WORKING", "工作中")),
                desk.amber,
                theme,
            ))
            .when(show_reasoning && !streaming.thinking.is_empty(), |this| {
                let id: SharedString = "streaming-thinking".into();
                let open = workspace.thinking_open(&id);
                let toggle_id = id.to_string();
                this.child(thinking_box(
                    id,
                    &streaming.thinking,
                    open,
                    theme,
                    cx.listener(move |workspace, _, _, cx| {
                        workspace.on_toggle_thinking(&toggle_id, cx);
                    }),
                ))
            })
            .when(!streaming.text.trim().is_empty(), |this| {
                this.child(status_line(
                    "streaming-agent-status",
                    t("AGENT", "代理"),
                    desk.green,
                    theme,
                ))
                .child(agent_text(
                    "streaming-agent-md".into(),
                    streaming.text.clone().into(),
                    theme,
                    true,
                ))
            })
            .when(
                streaming.thinking.trim().is_empty() && streaming.text.trim().is_empty(),
                |this| {
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
                },
            ),
    )
}

/// Desk transcript entries: a mono stamp gutter plus a ledger row per
/// entry. Assistant text is bare. User rows and tool blocks arrive
/// pre-routed by the caller's transcript collection and render through
/// their own builders.
pub(super) fn render_entry(
    entry: &ConversationEntry,
    workspace: &Workspace,
    theme: &Theme,
    show_reasoning: bool,
    cx: &Context<Workspace>,
) -> gpui_kit::AnyElement {
    match entry.kind {
        EntryKind::AssistantMessage => {
            let id = format!("thinking-{}", entry.event_id);
            let open = workspace.thinking_open(&id);
            let toggle_id = id;
            desk_block(
                entry,
                theme,
                assistant_block(
                    entry,
                    open,
                    show_reasoning,
                    theme,
                    cx.listener(move |workspace, _, _, cx| {
                        workspace.on_toggle_thinking(&toggle_id, cx);
                    }),
                ),
            )
            .into_any_element()
        }
        EntryKind::ToolResult => {
            let desk = Desk::of(theme);
            let failed = entry.text.starts_with("failed:");
            desk_block(
                entry,
                theme,
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
        EntryKind::Usage => div()
            .id(format!("entry-{}", entry.event_id))
            .into_any_element(),
        // Unreachable through the only caller: the transcript collection
        // routes user rows and tool calls to their own builders before any
        // entry reaches this match. The arm only keeps the match total.
        EntryKind::UserMessage | EntryKind::ToolCall => div().into_any_element(),
    }
}

/// Timeline block shell: a stamp gutter beside the entry content.
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

fn desk_shell(stamp: String, theme: &Theme, content: impl IntoElement) -> impl IntoElement {
    // `overflow_hidden` on this row, or on the `flex_1` column, collapses the
    // row inside the conversation scroller: the reply clips away and the
    // gutter paints as an empty separator. Wrapping text stays in the column
    // through `min_w_0`; the scroller clips the horizontal axis.
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_3()
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
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
        .child(div().flex_1().min_w_0().h_auto().child(content))
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
    show_reasoning: bool,
    theme: &Theme,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> impl IntoElement {
    let desk = Desk::of(theme);
    let thinking_id: SharedString = format!("thinking-{}", entry.event_id).into();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .when(show_reasoning && !entry.thinking.is_empty(), |this| {
            this.child(thinking_box(
                thinking_id,
                &entry.thinking,
                open,
                theme,
                on_toggle,
            ))
        })
        .when(!entry.text.trim().is_empty(), |this| {
            this.child(status_line(
                format!("agent-status-{}", entry.event_id),
                t("AGENT", "代理"),
                desk.green,
                theme,
            ))
            .child(agent_text(
                format!("agent-md-{}", entry.event_id).into(),
                SharedString::from(entry.text.trim()),
                theme,
                false,
            ))
        })
}

/// The centered transcript column inside the conversation scroller.
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
        .max_w(super::COLUMN_MAX)
        .mx_auto()
        .flex()
        .flex_col()
        .gap_3()
        .py_4()
        .px_4()
}

/// One collapsed header, `思考 · <summary> ▸`, or the same row with the trace open.
fn thinking_headline(label: &str, open: bool, text: &str) -> String {
    let marker = if open { "▾" } else { "▸" };
    let summary = thinking_summary(text);
    if summary.is_empty() {
        format!("{label} {marker}")
    } else {
        format!("{label} · {summary} {marker}")
    }
}

fn thinking_summary(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    ellipsis(line, 42)
}

/// Thinking trace. Distinct from the answer: a single header row toggles the
/// body. Collapsed, the header stays and the reply (a sibling) does not fold
/// with it.
///
/// The box is content-sized. `overflow_hidden` here lets the scroller treat
/// the row as a zero-minimum flex item and clip it down to a bar.
fn thinking_box(
    id: SharedString,
    text: &str,
    open: bool,
    theme: &Theme,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> impl IntoElement {
    let desk = Desk::of(theme);
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
        .gap_1()
        .rounded(px(8.))
        .border_l(px(2.))
        .border_color(desk.amber)
        .bg(desk.amber.opacity(0.10))
        .px_2()
        .py_1()
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
                .min_h(px(22.))
                .cursor_pointer()
                .rounded(px(6.))
                .hover(|row| row.bg(theme.secondary_hover.opacity(0.45)))
                .on_click(on_toggle)
                .child(
                    div()
                        .id(SharedString::from(format!("thinking-label-{toggle_id}")))
                        .min_w_0()
                        .h_auto()
                        .flex_none()
                        .text_xs()
                        .whitespace_nowrap()
                        .text_color(theme.muted_foreground)
                        .child(headline)
                        .test_support(),
                )
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
                    .whitespace_normal()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(text.to_owned())
                    .test_support(),
            )
        })
}

/// Assistant reply bubble: Markdown via gpui-kit's TextView.
///
/// Fenced code is highlighted by the kit's tree-sitter grammars
/// (`tree-sitter-languages`). `apply_palette` installs that highlighter
/// (`install_highlight`); the caller then publishes it with `Theme::sync_base`.
/// The id must be unique per entry — `ElementId::CodeLocation` would collide
/// across blocks since all bubbles render from the same call site.
fn agent_text(
    id: SharedString,
    text: SharedString,
    _theme: &Theme,
    stream_fade: bool,
) -> impl IntoElement {
    // The theme default leaves a full rem between paragraphs, which paints
    // as a tall empty slab when a reply is short or still streaming.
    let style = TextViewStyle::default().paragraph_gap(rems(0.35));
    div()
        .id(SharedString::from(format!("reply-shell-{id}")))
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .text_sm()
        .child(
            TextView::markdown(id, text)
                .style(style)
                .selectable(true)
                .stream_fade(stream_fade),
        )
        .test_support()
}

/// Gutter stamp: the last 8 characters of the entry id. The id is a ledger
/// identity, not a clock time.
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

/// One tool row. The summary wraps inside the column instead of widening it.
/// Clicking it reveals the result body without changing the tool call itself.
pub(super) fn render_tool_block(
    call: &ConversationEntry,
    result: Option<&ConversationEntry>,
    expanded: bool,
    cx: &Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
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
    let tool = tool_name(call.text.as_ref());
    let status = tool_status(tool, result);
    let event_id = call.event_id.clone();
    let row = div()
        .id(format!("tool-{}", call.event_id))
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .child(
            div()
                .id(format!("tool-summary-{}", call.event_id))
                .flex()
                .flex_row()
                .items_start()
                .gap_2()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .min_h(px(28.))
                .px_2()
                .py(px(4.))
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|row| row.bg(theme.secondary_hover.opacity(0.55)))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_toggle_tool_row(&event_id, cx);
                }))
                .child(
                    div()
                        .flex_shrink_0()
                        .pt(px(4.))
                        .child(crate::ui::lamp(lamp_color)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .w_full()
                        .overflow_hidden()
                        .whitespace_normal()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(tool_title(tool, call.text.as_ref()).to_string()),
                )
                .child(tool_status_node(&status, failed, theme)),
        );
    let row = if let (true, Some(result)) = (expanded, result) {
        row.child(
            div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .pt_1()
                .pl(px(16.))
                .child(result_body(tool, result, theme, &desk)),
        )
    } else {
        row
    };
    row.into_any_element()
}

fn tool_status(tool: &str, result: Option<&ConversationEntry>) -> String {
    let Some(result) = result else {
        return t("in progress", "进行中").to_owned();
    };
    let text = result.text.as_ref();
    if text.starts_with("failed:") {
        return t("failed", "失败").to_owned();
    }
    if tool_paints_diff(tool) {
        let added = text.lines().filter(|line| diff_added_line(line)).count();
        let removed = text.lines().filter(|line| diff_removed_line(line)).count();
        if added + removed > 0 {
            return format!("++{added} --{removed}");
        }
    }
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    if first.is_empty() {
        t("done", "完成").to_owned()
    } else {
        ellipsis(first, 48)
    }
}

/// Edit results use `++ ` / `-- ` markers. Page text from `fetch_content` and
/// `web_search` often starts lines with `- ` (a list), which is not a deletion.
/// Counting those lines painted a successful fetch as `DIFF ++0 --11`.
fn tool_paints_diff(tool: &str) -> bool {
    !matches!(tool, "fetch_content" | "web_search")
}

fn shows_diff_preview(tool: &str, text: &str) -> bool {
    tool_paints_diff(tool)
        && text.lines().any(|line| {
            diff_added_line(line) || diff_removed_line(line) || line.starts_with("[diff truncated]")
        })
}

/// The first token of a tool label is the tool name. The rest is the target.
fn tool_name(label: &str) -> &str {
    label.split_whitespace().next().unwrap_or(label)
}

/// `run_code` cards are titled by the program description. Other cards keep
/// the `name  target` label.
fn tool_title<'a>(tool: &str, label: &'a str) -> &'a str {
    if tool == "run_code" {
        label
            .split_once("  ")
            .map(|(_, rest)| rest.trim())
            .filter(|rest| !rest.is_empty())
            .unwrap_or(label)
    } else {
        label
    }
}

fn diff_added_line(line: &str) -> bool {
    (line.starts_with("++ ") || line.starts_with("+ ")) && !line.starts_with("+++")
}

fn diff_removed_line(line: &str) -> bool {
    (line.starts_with("-- ") || line.starts_with("- ")) && !line.starts_with("---")
}

fn diff_green() -> gpui_kit::Hsla {
    gpui_kit::rgb(0x3D_FF_9A).into()
}

fn diff_red() -> gpui_kit::Hsla {
    gpui_kit::rgb(0xFF_4D_5A).into()
}

fn tool_status_node(status: &str, failed: bool, theme: &Theme) -> gpui_kit::AnyElement {
    if let Some((added, removed)) = status.split_once(" --")
        && let Some(added) = added.strip_prefix("++")
    {
        return div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .child(div().text_color(diff_green()).child(format!("++{added}")))
            .child(div().text_color(diff_red()).child(format!("--{removed}")))
            .into_any_element();
    }
    div()
        .flex_shrink_0()
        .max_w(px(220.))
        .truncate()
        .text_xs()
        .text_color(if failed {
            diff_red()
        } else {
            theme.muted_foreground
        })
        .child(status.to_owned())
        .into_any_element()
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
    if shows_diff_preview(tool, &text) {
        diff_preview(&lines, theme, desk).into_any_element()
    } else if matches!(tool, "grep" | "find" | "search") && !failed {
        search_preview(tool, &lines, theme, desk).into_any_element()
    } else if failed || text.len() > 300 || lines.len() > 6 {
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
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
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .whitespace_normal()
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
    let added = lines.iter().filter(|line| diff_added_line(line)).count();
    let removed = lines.iter().filter(|line| diff_removed_line(line)).count();
    let shown = lines.len().min(80);
    div()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .py_1()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .child(
            div()
                .px_2()
                .py(px(2.))
                .flex()
                .flex_row()
                .gap_2()
                .font_family(theme.mono_font_family.clone())
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(t("DIFF", "差异")),
                )
                .child(div().text_color(diff_green()).child(format!("++{added}")))
                .child(div().text_color(diff_red()).child(format!("--{removed}"))),
        )
        .children(lines.iter().take(shown).map(|line| {
            let (color, bg) = if diff_added_line(line) {
                let ink = diff_green();
                (ink, ink.opacity(0.16))
            } else if diff_removed_line(line) {
                let ink = diff_red();
                (ink, ink.opacity(0.16))
            } else {
                (theme.muted_foreground, theme.transparent)
            };
            div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .whitespace_normal()
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
    let label = if tool == "find" {
        t("PATHS", "路径")
    } else {
        t("MATCHES", "匹配")
    };
    let shown = lines.len().min(40);
    div()
        .w_full()
        .min_w_0()
        .overflow_hidden()
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
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .whitespace_normal()
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
        .gap_1()
        .rounded(px(12.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted.opacity(0.65))
        .px_3()
        .py_2()
        .child(
            div()
                .text_xs()
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
    // Left-aligned ledger row. Hover edit/recall actions sit inline to the
    // right of the text.
    let mut column = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(status_line(
            format!("you-status-{}", entry.event_id),
            t("YOU", "你"),
            desk.cyan,
            theme,
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .w_full()
                .min_w_0()
                .rounded(px(12.))
                .bg(theme.accent.opacity(0.28))
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

#[cfg(test)]
mod tests {
    use super::{shows_diff_preview, tool_status};
    use crate::view_model::{ConversationEntry, EntryKind};

    fn result_entry(text: &str) -> ConversationEntry {
        ConversationEntry {
            event_id: "evt".into(),
            kind: EntryKind::ToolResult,
            text: text.into(),
            call_id: Some("call".into()),
            thinking: String::new(),
            parent_call_id: None,
        }
    }

    /// Eleven markdown list lines are what QA saw as `DIFF ++0 --11`.
    fn eleven_list_lines(header: &str) -> String {
        let mut page = format!("{header}\n");
        for n in 1..=11 {
            page.push_str(&format!("- line {n}\n"));
        }
        page
    }

    #[test]
    fn fetch_content_list_lines_are_not_a_diff() {
        let page = eleven_list_lines("[https://example.com/a]");
        let status = tool_status("fetch_content", Some(&result_entry(&page)));
        assert_eq!(status, "[https://example.com/a]");
        assert_ne!(status, "++0 --11");
        assert!(!shows_diff_preview("fetch_content", &page));
    }

    #[test]
    fn web_search_snippets_are_not_a_diff() {
        let text = "[https://example.com] Docs\n- alpha\n- beta\n";
        let status = tool_status("web_search", Some(&result_entry(text)));
        assert_eq!(status, "[https://example.com] Docs");
        assert!(!shows_diff_preview("web_search", text));
    }

    #[test]
    fn edit_removals_still_summarize_as_a_diff() {
        let mut text = String::from("edited src/lib.rs\n");
        for n in 1..=11 {
            text.push_str(&format!("-- line {n}\n"));
        }
        assert_eq!(tool_status("edit", Some(&result_entry(&text))), "++0 --11");
        assert!(shows_diff_preview("edit", &text));
    }

    #[test]
    fn collapsed_thinking_headline_is_one_summary_row() {
        let trace = "reasoning step 0 weighs the next edit\nsecond line";
        assert_eq!(
            super::thinking_headline("思考", false, trace),
            "思考 · reasoning step 0 weighs the next edit ▸"
        );
        assert_eq!(
            super::thinking_headline("Thinking", true, trace),
            "Thinking · reasoning step 0 weighs the next edit ▾"
        );
        assert_eq!(super::thinking_headline("思考", false, "   "), "思考 ▸");
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
            parent_call_id: None,
        }
    }

    impl Render for TranscriptProbe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                        &theme,
                        assistant_block(
                            entry,
                            expanded,
                            true,
                            &theme,
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
}
