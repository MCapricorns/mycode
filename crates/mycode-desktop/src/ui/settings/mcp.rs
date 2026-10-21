//! The MCP settings page: configured server rows, the read-only servers
//! magpie's Library writes, JSON import, and the custom add-server form.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputState, Textarea};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{DropdownSpec, dropdown_field, labeled_field, settings_card};
use crate::i18n::t;
use crate::ui::desk::Desk;
use crate::ui::skin;
use crate::view_model::FrontendAction;
use crate::workspace::Workspace;

pub(super) fn render_mcp_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match workspace.vm().mcp_subview {
        crate::view_model::McpSubview::List => render_mcp_list(workspace, cx),
        crate::view_model::McpSubview::Json => render_mcp_json_page(workspace, window, cx),
        crate::view_model::McpSubview::Custom => render_mcp_custom_page(workspace, window, cx),
    }
}

fn render_mcp_list(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let mcp_rows = {
        let vm = workspace.vm();
        let Some(settings) = vm.settings.as_ref() else {
            return div().into_any_element();
        };
        let rows: Vec<McpRow> = settings
            .mcp_servers
            .iter()
            .enumerate()
            .map(|(index, server)| {
                let target = match server.transport.as_str() {
                    "stdio" => {
                        let mut line = server.command.clone().unwrap_or_default();
                        for arg in &server.args {
                            line.push(' ');
                            // Quoted like the form accepts it, so an argument
                            // with spaces does not read as several.
                            if arg.contains(char::is_whitespace) {
                                line.push('"');
                                line.push_str(arg);
                                line.push('"');
                            } else {
                                line.push_str(arg);
                            }
                        }
                        line
                    }
                    _ => server.endpoint.clone().unwrap_or_default(),
                };
                McpRow {
                    id: server.id.clone(),
                    transport: server.transport.clone(),
                    target,
                    enabled: server.enabled,
                    keyed: settings.mcp_with_keys.iter().any(|id| id == &server.id),
                    index,
                    tools: vm
                        .mcp_tools
                        .iter()
                        .find(|(id, _)| *id == server.id)
                        .map(|(_, tools)| tools.clone()),
                    probing: vm.mcp_probing.contains(&server.id),
                    armed: workspace.delete_armed(&format!("mcp:{}", server.id)),
                }
            })
            .collect();
        rows
    };
    let theme = cx.theme();
    let mcp_row_elements: Vec<AnyElement> =
        mcp_rows.into_iter().map(|row| mcp_row(row, cx)).collect();
    let magpie_servers = mycode_config::global_mcp_servers(workspace.home()).unwrap_or_default();
    let magpie_rows: Vec<AnyElement> = magpie_servers
        .iter()
        .map(|server| {
            let target = match server.transport.as_str() {
                "stdio" => server.command.clone().unwrap_or_default(),
                _ => server.endpoint.clone().unwrap_or_default(),
            };
            div()
                .id(format!("mcp-magpie-{}", server.id))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .py_1()
                .border_b_1()
                .border_color(theme.border)
                .child(Icon::new(IconName::Lock).xsmall())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_sm().child(server.id.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(target),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("magpie"),
                )
                .into_any_element()
        })
        .collect();

    let mcp_empty = mcp_row_elements.is_empty();
    div()
        .id("mcp-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "mcp",
            t("MCP servers", "MCP 服务器"),
            Some(t(
                "Stdio or Streamable-HTTP tool servers. A stored key shows as a lock and \
                 is never written back into a field. Enabling a server probes it first; \
                 once enabled, its tools join the agent.",
                "Stdio 或 Streamable-HTTP 工具服务器。已保存的密钥显示为锁形标记，且不会回填到输入框。\
                 启用前会先探测服务器；启用后其工具随即加入代理。",
            )),
            theme,
            vec![
                div()
                    .when(mcp_empty, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(t("No MCP servers yet", "还没有 MCP 服务器")),
                        )
                    })
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(mcp_row_elements)
                    .into_any_element(),
                div()
                    .id("add-mcp-row")
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("mcp-open-json")
                            .icon(IconName::File)
                            .label(t("Import JSON\u{2026}", "导入 JSON\u{2026}"))
                            .small()
                            .outline()
                            .on_click(cx.listener(|workspace, _, _, cx| {
                                workspace
                                    .on_show_mcp_subview(crate::view_model::McpSubview::Json, cx);
                            })),
                    )
                    .child(
                        Button::new("mcp-open-custom")
                            .icon(IconName::Terminal)
                            .label(t("Add custom server\u{2026}", "添加自定义服务器\u{2026}"))
                            .small()
                            .outline()
                            .on_click(cx.listener(|workspace, _, _, cx| {
                                workspace
                                    .on_show_mcp_subview(crate::view_model::McpSubview::Custom, cx);
                            })),
                    )
                    .into_any_element(),
            ],
        ))
        .child(settings_card(
            "mcp-magpie",
            t("From magpie", "来自 magpie"),
            Some(t(
                "magpie's Library writes ~/.mycode/mcp.json. These rows are read-only here; \
                 a server you configure with the same id wins.",
                "magpie 的 Library 写入 ~/.mycode/mcp.json。这些行在这里是只读的；\
                 同 id 的自有服务器优先。",
            )),
            theme,
            vec![if magpie_rows.is_empty() {
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
                    .children(magpie_rows)
                    .into_any_element()
            }],
        ))
        .into_any_element()
}

fn render_mcp_json_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let json_input = workspace.mcp_json_input(window, cx);
    let header = super::subview_header(
        t("MCP", "MCP"),
        t("Import JSON", "导入 JSON"),
        Some(t(
            "Claude Desktop or Cursor mcp.json",
            "Claude Desktop 或 Cursor 的 mcp.json",
        )),
        |workspace, cx| {
            workspace.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
        },
        cx,
    );
    let theme = cx.theme();
    div()
        .id("mcp-json-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(settings_card(
            "mcp-import",
            t("Paste a config", "粘贴配置"),
            Some(t(
                "A servers map or one server object. Authorization headers are stored as \
                 the API key and are not shown again.",
                "可以是 servers 映射或单个服务器对象。Authorization 头会作为 API 密钥保存，\
                 之后不再显示。",
            )),
            theme,
            vec![
                div()
                    .min_h(px(96.))
                    .w_full()
                    .child(Textarea::new(&json_input))
                    .into_any_element(),
                Button::new("mcp-import-json")
                    .label(t("Import pasted JSON", "导入粘贴的 JSON"))
                    .small()
                    .outline()
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.on_import_mcp_json(window, cx);
                    }))
                    .into_any_element(),
            ],
        ))
        .into_any_element()
}

fn render_mcp_custom_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let form = render_mcp_form(workspace, window, cx);
    div()
        .id("mcp-custom-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(super::subview_header(
            t("MCP", "MCP"),
            t("Add custom server", "添加自定义服务器"),
            Some(t(
                "stdio command or Streamable HTTP",
                "stdio 命令或 Streamable HTTP",
            )),
            |workspace, cx| {
                workspace.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
            },
            cx,
        ))
        .child(form)
        .into_any_element()
}

/// One configured MCP server as the settings row shows it.
struct McpRow {
    id: String,
    transport: String,
    /// Endpoint URL or the full stdio command line.
    target: String,
    enabled: bool,
    keyed: bool,
    index: usize,
    /// Tool names from the last probe, when one succeeded.
    tools: Option<Vec<String>>,
    probing: bool,
    /// The trash was clicked once and waits for the confirming click.
    armed: bool,
}

fn mcp_row(row: McpRow, cx: &Context<Workspace>) -> AnyElement {
    let McpRow {
        id,
        transport,
        target,
        enabled,
        keyed,
        index,
        tools,
        probing,
        armed,
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
        .id(format!("mcp-row-{id}"))
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
                        .id(format!("mcp-key-lock-{id}"))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(22.))
                        .text_color(if keyed {
                            theme.success
                        } else {
                            theme.muted_foreground
                        })
                        .child(Icon::new(IconName::Lock).small()),
                )
                .child(
                    Switch::new(format!("mcp-toggle-{id}"))
                        .checked(enabled)
                        .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                            workspace.on_mcp_toggled(index, *checked, cx);
                        })),
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
                )
                .map(|this| {
                    let key = format!("mcp:{id}");
                    let on_remove = cx.listener(move |workspace, _, _, cx| {
                        if workspace.confirm_delete(&key, cx) {
                            workspace.apply_action(FrontendAction::SettingsMcpRemoved(index), cx);
                        }
                    });
                    if armed {
                        this.child(
                            div()
                                .id(format!("mcp-remove-{id}"))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .px_2()
                                .h(px(24.))
                                .rounded(px(4.))
                                .flex_shrink_0()
                                .cursor_pointer()
                                .bg(theme.danger.opacity(0.12))
                                .text_color(theme.danger)
                                .text_xs()
                                .on_click(on_remove)
                                .child(Icon::new(IconName::Trash).xsmall())
                                .child(t("Remove?", "移除？")),
                        )
                    } else {
                        this.child(crate::ui::icon_button(
                            format!("mcp-remove-{id}"),
                            IconName::Trash,
                            on_remove,
                            cx,
                        ))
                    }
                }),
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

/// Inline add-MCP-server form state (http or stdio).
pub(crate) struct McpForm {
    /// Server identity input.
    pub id: Entity<InputState>,
    /// Selected transport (`http` or `stdio`).
    pub transport: String,
    /// HTTP endpoint input (http transport).
    pub endpoint: Entity<InputState>,
    /// Full command line input (stdio transport): program plus arguments.
    pub command: Entity<InputState>,
    /// Extra environment variables (stdio transport), `KEY=VALUE` pairs.
    pub env: Entity<InputState>,
    /// API key input, stored in the secret store.
    pub api_key: Entity<InputState>,
}

impl McpForm {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<Self> {
        let mut make = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let id = make("id, e.g. my-mcp");
        let endpoint = make("https://mcp.example.com/mcp");
        let command = make("npx -y @modelcontextprotocol/server-filesystem C:\\projects");
        let env = make("KEY=value, OTHER=value (optional)");
        let api_key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("api key (leave empty to skip)")
                .masked(true)
        });
        cx.new(|_| Self {
            id,
            transport: "http".to_owned(),
            endpoint,
            command,
            env,
            api_key,
        })
    }

    pub(crate) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.id
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.transport = "http".to_owned();
        self.endpoint
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.command
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.env
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.api_key
            .update(cx, |state, cx| state.set_value("", window, cx));
    }
}

fn render_mcp_form(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let form = workspace.mcp_form(window, cx);
    let transport = form.read(cx).transport.clone();
    let is_http = transport == "http";
    let transport_menu_open = workspace.vm().mcp_transport_menu_open;
    let transport_options = vec!["http".to_owned(), "stdio".to_owned()];
    let transport_field = dropdown_field(
        DropdownSpec {
            id: "mcp-transport",
            label: t("Transport", "传输方式"),
            description: Some(t(
                "HTTP servers speak Streamable-HTTP; stdio servers spawn a command.",
                "HTTP 服务器使用 Streamable-HTTP；stdio 服务器启动一条命令。",
            )),
            current: &transport,
            options: &transport_options,
            open: transport_menu_open,
        },
        |workspace, open, cx| workspace.on_toggle_mcp_transport_menu(open, cx),
        |workspace, transport, cx| workspace.on_select_mcp_transport(transport, cx),
        cx,
    );
    let (id_input, endpoint_input, api_key_input, command_input, env_input) = {
        let read = form.read(cx);
        (
            read.id.clone(),
            read.endpoint.clone(),
            read.api_key.clone(),
            read.command.clone(),
            read.env.clone(),
        )
    };
    let theme: &Theme = cx.theme();
    settings_card(
        "mcp-form",
        t("Add a custom MCP server", "添加自定义 MCP 服务器"),
        Some(t(
            "Use a unique id. HTTP servers require an https:// endpoint.",
            "请使用唯一标识。HTTP 服务器需要 https:// 端点。",
        )),
        theme,
        vec![
            div()
                .flex()
                .flex_col()
                .gap_2()
                .text_sm()
                .child(labeled_field(t("id", "标识"), id_input, theme))
                .child(transport_field)
                // Only the fields the chosen transport uses are shown; the
                // old form listed every field and users typed the full
                // command line into a single "command" that never resolved.
                .when(is_http, |this| {
                    this.child(labeled_field(
                        t("endpoint (https URL)", "端点（https URL）"),
                        endpoint_input,
                        theme,
                    ))
                    .child(labeled_field(
                        t(
                            "api key (stored in the vault, sent as Bearer)",
                            "API 密钥（保存在凭据库，以 Bearer 发送）",
                        ),
                        api_key_input,
                        theme,
                    ))
                })
                .when(!is_http, |this| {
                    this.child(labeled_field(
                        t(
                            "command line (program and arguments, quotes allowed)",
                            "命令行（程序与参数，允许引号）",
                        ),
                        command_input,
                        theme,
                    ))
                    .child(labeled_field(
                        t(
                            "environment (KEY=VALUE pairs, optional)",
                            "环境变量（KEY=VALUE，可选）",
                        ),
                        env_input,
                        theme,
                    ))
                })
                .into_any_element(),
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    Button::new("mcp-add")
                        .icon(IconName::Check)
                        .label(t("Add server", "添加服务器"))
                        .small()
                        .outline()
                        .on_click(cx.listener(|workspace, _, window, cx| {
                            workspace.on_add_mcp(window, cx);
                        })),
                )
                .into_any_element(),
        ],
    )
    .into_any_element()
}
