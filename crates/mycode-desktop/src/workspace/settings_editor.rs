//! The settings editor half of the workspace: form entities, MCP forms,
//! subagent routes, and the save pipeline they all feed.
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::{AppContext as _, Context, Entity, Window};

use mycode_frontend::forms::DEFAULT_PROXY_URL;

use crate::ui::McpForm;
use crate::view_model::FrontendAction;
use crate::workspace::Workspace;

mod inputs;

impl Workspace {
    /// The proxy URL field. Only a value that parses as a proxy reaches the
    /// settings document, so a half-typed URL never fails a save.
    pub(crate) fn settings_proxy_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.proxy_input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(DEFAULT_PROXY_URL));
            cx.subscribe_in(
                &input,
                window,
                |workspace, entity, event, _, cx| match event {
                    InputEvent::Change => {
                        let text = entity.read(cx).value().to_string();
                        workspace.driver_do(|d, fx| d.proxy_url_typed(&text, fx), cx);
                    }
                    InputEvent::Blur => {
                        workspace.driver_do(|d, fx| d.proxy_url_committed(fx), cx);
                    }
                    InputEvent::Focus | InputEvent::PressEnter { .. } => {}
                },
            )
            .detach();
            self.proxy_input = Some(input);
        }
        let input = self.proxy_input.clone().expect("proxy input");
        if self.proxy_sync_pending {
            let target = self
                .vm()
                .settings
                .as_ref()
                .map(|settings| settings.proxy.clone())
                .filter(|proxy| {
                    matches!(
                        mycode_config::ProxyPreference::parse(proxy),
                        Some(mycode_config::ProxyPreference::Url(_))
                    )
                })
                .unwrap_or_default();
            let current = input.read(cx).value().to_string();
            if current != target {
                input.update(cx, |state, cx| state.set_value(target, window, cx));
            }
            self.proxy_sync_pending = false;
        }
        input
    }

    /// Picks the proxy mode from the General page: `system` or `manual`.
    /// Manual keeps a stored URL, or starts from Clash's default port.
    pub(crate) fn on_select_proxy_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.select_proxy_mode(mode, fx), cx);
    }

    /// The General page's "Test" button: fetch a probe URL through the
    /// proxy value on screen.
    pub(crate) fn on_test_proxy(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.test_proxy(fx), cx);
    }

    // ---- MCP servers ----

    pub(crate) fn mcp_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<McpForm> {
        self.mcp_form
            .get_or_insert_with(|| McpForm::new(window, cx))
            .clone()
    }

    /// Adds the server the custom form currently describes: the frontend's
    /// `forms::build_mcp_server` validates the fields, this method dispatches.
    pub(crate) fn on_add_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.mcp_form.clone() else {
            return;
        };
        let (id, transport, endpoint, command, env_line, api_key) = {
            let read = form.read(cx);
            (
                read.id.read(cx).value().trim().to_string(),
                read.transport.clone(),
                read.endpoint.read(cx).value().trim().to_string(),
                read.command.read(cx).value().trim().to_string(),
                read.env.read(cx).value().trim().to_string(),
                read.api_key.read(cx).value().to_string(),
            )
        };
        let mut added = false;
        self.driver_do(
            |d, fx| {
                added = d.add_custom_mcp(
                    &id, &transport, &endpoint, &command, &env_line, &api_key, fx,
                );
            },
            cx,
        );
        if added {
            form.update(cx, |form, cx| form.clear(window, cx));
        }
    }

    pub(crate) fn mcp_json_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        self.mcp_json_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder(crate::i18n::t(
                            "Paste mcp.json or a Claude Desktop / Cursor config…",
                            "粘贴 mcp.json，或 Claude Desktop / Cursor 配置…",
                        ))
                        .auto_grow(4, 16)
                })
            })
            .clone()
    }

    pub(crate) fn on_import_mcp_json(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.mcp_json_input.clone() else {
            return;
        };
        let raw = input.read(cx).value().to_string();
        let mut parsed = false;
        self.driver_do(|d, fx| parsed = d.import_mcp_json(&raw, fx), cx);
        if parsed {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    // ---- skills ----

    pub(crate) fn on_skill_enabled(&mut self, slug: &str, enabled: bool, cx: &mut Context<Self>) {
        self.apply_action(
            FrontendAction::SettingsSkillToggled(slug.to_owned(), enabled),
            cx,
        );
    }

    // ---- subagent routes ----

    pub(crate) fn on_subagent_role_enabled(
        &mut self,
        role: &str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.driver_do(|d, fx| d.toggle_subagent_role(role, enabled, fx), cx);
    }

    pub(crate) fn on_toggle_subagent_menu(
        &mut self,
        role: &str,
        field: &str,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        let next = open.then(|| (role.to_owned(), field.to_owned()));
        self.apply_action(FrontendAction::SubagentMenuToggled(next), cx);
    }

    pub(crate) fn on_set_subagent_thinking(
        &mut self,
        role: &str,
        thinking: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.driver_do(|d, fx| d.set_subagent_thinking(role, thinking, fx), cx);
    }

    pub(crate) fn on_set_subagent_route(
        &mut self,
        role: &str,
        provider: Option<String>,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.driver_do(|d, fx| d.set_subagent_route(role, provider, model, fx), cx);
    }
}
