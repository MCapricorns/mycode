//! The MCP settings page: the read-only servers magpie's Library writes to
//! `~/.mycode/mcp.json`, with a tools probe per row.
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div, px,
};

use super::widgets::settings_card;
use crate::i18n::t;
use crate::ui::desk::Desk;
use crate::ui::skin;
use crate::workspace::Workspace;

pub(super) fn render_mcp_section(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    render_mcp_list(workspace, cx)
}

fn render_mcp_list(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    // magpie's own file is read into the view model, so this page, the nav
    // count, and the composer's `/` menu show one catalog.
    let library_error = workspace.vm().mcp_library_error.clone();
    let magpie_servers = workspace.vm().mcp_library.clone();
    let theme = cx.theme();
    let magpie_rows: Vec<AnyElement> = magpie_servers
        .iter()
        .map(|server| {
            let id = server.id.clone();
            let target = match server.transport.as_str() {
                "stdio" => {
                    let mut line = server.command.clone().unwrap_or_default();
                    for arg in &server.args {
                        line.push(' ');
                        line.push_str(arg);
                    }
                    line
                }
                _ => server.endpoint.clone().unwrap_or_default(),
            };
            let tools = workspace
                .vm()
                .mcp_tools
                .iter()
                .find(|(tool_id, _)| *tool_id == server.id)
                .map(|(_, tools)| tools.clone());
            let probing = workspace.vm().mcp_probing.contains(&server.id);
            library_row(
                LibraryRow {
                    id,
                    transport: server.transport.clone(),
                    target,
                    enabled: server.enabled,
                    tools,
                    probing,
                },
                cx,
            )
        })
        .collect();

    div()
        .id("mcp-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "mcp-magpie",
            t("MCP servers from magpie", "来自 magpie 的 MCP 服务器"),
            Some(t(
                "magpie's Library writes ~/.mycode/mcp.json. The rows are read-only here; \
                 enable or edit servers in magpie. Probe reads a server's live tool list.",
                "magpie 的 Library 写入 ~/.mycode/mcp.json。这里的行是只读的；\
                 启用或编辑服务器请在 magpie 中操作。探测读取服务器的实时工具列表。",
            )),
            theme,
            vec![if let Some(error) = library_error {
                div()
                    .id("mcp-magpie-error")
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(error)
                    .into_any_element()
            } else if magpie_rows.is_empty() {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t(
                        "No library servers. magpie adds them from its Library page.",
                        "还没有 Library 服务器。magpie 会在 Library 页添加。",
                    ))
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(magpie_rows)
                    .into_any_element()
            }],
        ))
        .into_any_element()
}

/// One magpie Library server as the settings row shows it.
struct LibraryRow {
    id: String,
    transport: String,
    /// Endpoint URL or the full stdio command line.
    target: String,
    enabled: bool,
    /// Tool names from the last probe, when one succeeded.
    tools: Option<Vec<String>>,
    probing: bool,
}

fn library_row(row: LibraryRow, cx: &Context<Workspace>) -> AnyElement {
    let LibraryRow {
        id,
        transport,
        target,
        enabled,
        tools,
        probing,
    } = row;
    let theme = cx.theme();
    let desk = Desk::of(theme);
    let tool_chips: Vec<AnyElement> = tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|tool| {
            div()
                .px(px(6.))
                .py(px(1.))
                .rounded(skin::radius_xs())
                .border_1()
                .border_color(theme.border)
                .bg(skin::frost(theme))
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .child(tool.clone())
                .into_any_element()
        })
        .collect();
    let tools_summary: Option<String> = tools.as_ref().map(|tools| {
        if tools.is_empty() {
            format!(
                "{} \u{b7} {}",
                t("connected", "已连接"),
                t("no tools advertised", "未提供工具")
            )
        } else {
            format!(
                "{} \u{b7} {} {}",
                t("connected", "已连接"),
                tools.len(),
                t("tool(s)", "个工具")
            )
        }
    });
    div()
        .id(format!("mcp-magpie-{id}"))
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .rounded(skin::radius_control())
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::frost(theme))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(crate::ui::lamp(if tools.is_some() {
                    desk.green
                } else if enabled {
                    desk.amber
                } else {
                    desk.faint
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                                .child(id.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .whitespace_normal()
                                .child(format!("{transport} \u{b7} {target}")),
                        )
                        .when_some(tools_summary, |this, summary| {
                            this.child(div().text_xs().text_color(desk.green).child(summary))
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if enabled {
                            t("enabled", "已启用")
                        } else {
                            t("disabled in magpie", "已在 magpie 停用")
                        }),
                )
                .child(
                    Button::new(format!("mcp-tools-{id}"))
                        .label(if probing {
                            t("Connecting\u{2026}", "连接中\u{2026}")
                        } else {
                            t("Probe", "探测")
                        })
                        .small()
                        .ghost()
                        .disabled(probing)
                        .on_click({
                            let id = id.clone();
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_list_mcp_tools(&id, cx);
                            })
                        }),
                ),
        )
        .when(!tool_chips.is_empty(), |this| {
            this.child(
                div()
                    .id(format!("mcp-tools-list-{id}"))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .pl(px(19.))
                    .children(tool_chips),
            )
        })
        .into_any_element()
}
