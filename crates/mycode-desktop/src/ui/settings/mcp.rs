//! The MCP settings page: configured server rows, the built-in catalog, JSON
//! import, and the custom add-server form (plus its pure row builder).
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Styled, Window, div, px,
};

use super::widgets::{dropdown_field, labeled_field, settings_card};
use crate::i18n::t;
use crate::ui::desk::Desk;
use crate::ui::skin;
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

pub(super) fn render_mcp_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match workspace.vm().mcp_subview {
        crate::view_model::McpSubview::List => render_mcp_list(workspace, cx),
        crate::view_model::McpSubview::Catalog => render_mcp_catalog_page(workspace, window, cx),
        crate::view_model::McpSubview::Json => render_mcp_json_page(workspace, window, cx),
        crate::view_model::McpSubview::Custom => render_mcp_custom_page(workspace, window, cx),
    }
}

fn render_mcp_list(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let listed_tools = workspace.vm().mcp_tools.clone();
    let probing = workspace.vm().mcp_probing.clone();
    let mcp_rows: Vec<McpRow> = settings
        .mcp_servers
        .iter()
        .enumerate()
        .map(|(index, server)| {
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
            McpRow {
                id: server.id.clone(),
                transport: server.transport.clone(),
                target,
                enabled: server.enabled,
                keyed: settings.mcp_with_keys.iter().any(|id| id == &server.id),
                index,
                tools: listed_tools
                    .iter()
                    .find(|(id, _)| *id == server.id)
                    .map(|(_, tools)| tools.clone()),
                probing: probing.contains(&server.id),
            }
        })
        .collect();
    let has_catalog = mycode_config::builtin_mcp_servers().iter().any(|server| {
        !settings
            .mcp_servers
            .iter()
            .any(|configured| configured.id == server.id)
    });
    let mcp_row_elements: Vec<AnyElement> =
        mcp_rows.into_iter().map(|row| mcp_row(row, cx)).collect();

    let theme = cx.theme();
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
                 is never written back into a field. Enabled servers connect once and \
                 their tools join the agent.",
                "Stdio 或 Streamable-HTTP 工具服务器。已保存的密钥显示为锁形标记,且不会回填到输入框。\
                 启用的服务器会连接一次,其工具随即加入代理。",
            )),
            theme,
            vec![
                div()
                    .when(mcp_empty, |this| {
                        this.child(div().text_xs().opacity(0.5).child(t("No MCP servers yet", "还没有 MCP 服务器")))
                    })
                    .children(mcp_row_elements)
                    .into_any_element(),
                div()
                    .id("add-mcp-row")
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .pt_1()
                    .when(has_catalog, |this| {
                        this.child(
                            Button::new("mcp-open-catalog")
                                .icon(IconName::Plus)
                                .label(t("Add from catalog\u{2026}", "从目录添加\u{2026}"))
                                .small()
                                .primary()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_mcp_subview(
                                        crate::view_model::McpSubview::Catalog,
                                        cx,
                                    );
                                })),
                        )
                    })
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
        .into_any_element()
}

fn render_mcp_catalog_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let catalog_rows: Vec<(String, String)> = mycode_config::builtin_mcp_servers()
        .into_iter()
        .filter(|server| {
            !settings
                .mcp_servers
                .iter()
                .any(|configured| configured.id == server.id)
        })
        .map(|server| (server.id, server.transport))
        .collect();
    let catalog_row_elements: Vec<AnyElement> = catalog_rows
        .iter()
        .map(|(id, transport)| builtin_catalog_row(id, transport, cx))
        .collect();
    let key_input = workspace.mcp_key_input(window, cx);
    let header = super::subview_header(
        t("MCP", "MCP"),
        t("Add from catalog", "从目录添加"),
        Some(t(
            "Paste a new key, then Add. A stored key is not shown here.",
            "粘贴新的密钥后点击添加。已保存的密钥不会在这里显示。",
        )),
        |workspace, cx| {
            workspace.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
        },
        cx,
    );
    let theme = cx.theme();
    div()
        .id("mcp-catalog-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(settings_card(
            "mcp-catalog",
            t("Built-in servers", "内置服务器"),
            Some(t(
                "The key field starts empty. Bearer is added on the wire.",
                "密钥输入框默认为空。Bearer 会在请求时自动附加。",
            )),
            theme,
            vec![
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(catalog_row_elements)
                    .child(
                        div()
                            .id("mcp-key-row")
                            .flex()
                            .flex_col()
                            .gap_1()
                            .pt_2()
                            .child(
                                div()
                                    .text_xs()
                                    .opacity(0.6)
                                    .child(t("Key for the server you add", "要添加的服务器的密钥")),
                            )
                            .child(div().h(px(28.)).text_sm().child(Input::new(&key_input))),
                    )
                    .into_any_element(),
            ],
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
                "可以是 servers 映射或单个服务器对象。Authorization 头会作为 API 密钥保存,\
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
                    .primary()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_import_mcp_json(cx);
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
                .rounded(px(3.))
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
        .rounded_md()
        .border_1()
        .border_color(theme.border)
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
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(id.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .opacity(0.6)
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
                            workspace.apply_action(
                                DesktopAction::SettingsMcpToggled(index, *checked),
                                cx,
                            );
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
                .child(crate::ui::icon_button(
                    format!("mcp-remove-{id}"),
                    IconName::Trash,
                    cx.listener(move |workspace, _, _, cx| {
                        workspace.apply_action(DesktopAction::SettingsMcpRemoved(index), cx);
                    }),
                    cx,
                )),
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

fn builtin_catalog_row(id: &str, transport: &str, cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .id(format!("mcp-catalog-{id}"))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_sm()
                .opacity(0.8)
                .child(format!("{id} \u{b7} {transport}")),
        )
        .child(
            Button::new(format!("mcp-add-{id}"))
                .label(t("Add", "添加"))
                .small()
                .primary()
                .on_click({
                    let id = id.to_owned();
                    cx.listener(move |workspace, _, window, cx| {
                        let server = mycode_config::builtin_mcp_servers()
                            .into_iter()
                            .find(|server| server.id == id)
                            .expect("catalog entry");
                        let input = workspace.mcp_key_input(window, cx);
                        let key = input.read(cx).value().trim().to_owned();
                        input.update(cx, |state, cx| state.set_value("", window, cx));
                        workspace.on_add_builtin_mcp(server, &key, cx);
                    })
                }),
        )
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
        let api_key = make("api key (leave empty to skip)");
        cx.new(|_| Self {
            id,
            transport: "http".to_owned(),
            endpoint,
            command,
            env,
            api_key,
        })
    }
}

/// Builds the server row the custom form currently describes, from its raw
/// field values. Pure validation and construction only; the workspace
/// method decides what to do with the result.
pub(crate) fn build_mcp_server(
    id: &str,
    transport: &str,
    endpoint: &str,
    command: &str,
    env_line: &str,
) -> Result<mycode_config::McpServerSettings, String> {
    match transport {
        "http" => {
            if endpoint.is_empty() {
                return Err("http servers need an endpoint".to_owned());
            }
            Ok(mycode_config::McpServerSettings {
                id: id.to_owned(),
                enabled: true,
                transport: transport.to_owned(),
                command: None,
                args: Vec::new(),
                env: Default::default(),
                endpoint: Some(endpoint.to_owned()),
                key_header: Some("bearer".to_owned()),
            })
        }
        "stdio" => {
            // The form takes the whole line the server docs publish
            // (`npx -y @scope/server --flag`); split it here so the
            // child gets a program plus argv, not one giant program name.
            let mut words = mycode_config::split_command_line(command).into_iter();
            let Some(program) = words.next().filter(|word| !word.is_empty()) else {
                return Err("stdio servers need a command".to_owned());
            };
            let env = crate::workspace::parse_env_line(env_line)?;
            Ok(mycode_config::McpServerSettings {
                id: id.to_owned(),
                enabled: true,
                transport: transport.to_owned(),
                command: Some(program),
                args: words.collect(),
                env,
                endpoint: None,
                key_header: None,
            })
        }
        _ => Err("transport must be http or stdio".to_owned()),
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
        "mcp-transport",
        t("Transport", "传输方式"),
        Some(t(
            "HTTP servers speak Streamable-HTTP; stdio servers spawn a command.",
            "HTTP 服务器使用 Streamable-HTTP;stdio 服务器启动一条命令。",
        )),
        &transport,
        &transport_options,
        transport_menu_open,
        |workspace, open, cx| workspace.on_toggle_mcp_transport_menu(open, cx),
        |workspace, transport, cx| workspace.on_select_mcp_transport(transport, cx),
        cx,
    );
    let theme: &Theme = cx.theme();
    div()
        .id("mcp-form")
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(theme.secondary)
        .child(
            div()
                .text_xs()
                .opacity(0.7)
                .child(t("Add a custom MCP server", "添加自定义 MCP 服务器")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .text_sm()
                .child(labeled_field(t("id", "标识"), form.read(cx).id.clone()))
                .child(transport_field)
                // Only the fields the chosen transport uses are shown; the
                // old form listed every field and users typed the full
                // command line into a single "command" that never resolved.
                .when(is_http, |this| {
                    this.child(labeled_field(
                        t("endpoint (https URL)", "端点(https URL)"),
                        form.read(cx).endpoint.clone(),
                    ))
                    .child(labeled_field(
                        t(
                            "api key (stored in the vault, sent as Bearer)",
                            "API 密钥(保存在凭据库,以 Bearer 发送)",
                        ),
                        form.read(cx).api_key.clone(),
                    ))
                })
                .when(!is_http, |this| {
                    this.child(labeled_field(
                        t(
                            "command line (program and arguments, quotes allowed)",
                            "命令行(程序与参数,允许引号)",
                        ),
                        form.read(cx).command.clone(),
                    ))
                    .child(labeled_field(
                        t(
                            "environment (KEY=VALUE pairs, optional)",
                            "环境变量(KEY=VALUE,可选)",
                        ),
                        form.read(cx).env.clone(),
                    ))
                }),
        )
        .child(
            Button::new("mcp-add")
                .label(t("Add server", "添加服务器"))
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_add_mcp(cx);
                })),
        )
        .into_any_element()
}
