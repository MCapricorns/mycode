//! The settings editor half of the workspace: form entities, backend rows,
//! MCP forms, subagent routes, and the save pipeline they all feed. Provider
//! rows live in [`providers`].
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::{AppContext as _, Context, Entity, Window};

use mycode_app::BridgeCommand;

use crate::ui::{McpForm, build_mcp_server};
use crate::view_model::FrontendAction;
use crate::workspace::Workspace;

mod providers;

/// Trims an API key candidate for UI button enablement and save dispatch.
///
/// Saving an empty or whitespace-only key field is a no-op so the replace-key
/// flow cannot delete a stored credential.
pub(crate) fn normalized_key_input(raw: &str) -> Option<String> {
    let key = mycode_config::normalize_api_key(raw);
    (!key.is_empty() && !key.eq_ignore_ascii_case("bearer")).then_some(key)
}

/// The URL the custom proxy mode starts from: Clash's mixed port.
pub(crate) const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:7890";

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
                        let text = entity.read(cx).value().trim().to_owned();
                        if matches!(
                            mycode_config::ProxyPreference::parse(&text),
                            Some(mycode_config::ProxyPreference::Url(_))
                        ) {
                            workspace.apply_action(
                                FrontendAction::SettingsProxyChanged {
                                    value: text,
                                    typed: true,
                                },
                                cx,
                            );
                        }
                    }
                    InputEvent::Blur => workspace.flush_settings_text_save(cx),
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

    /// Picks the proxy mode from the General page: `auto`, `direct`, or
    /// `custom`. Custom keeps a stored URL, or starts from Clash's default
    /// port.
    pub(crate) fn on_select_proxy_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.select_proxy_mode(mode, fx), cx);
    }

    // ---- web search backends ----

    /// The row's enable switch. Disabling is immediate; enabling first runs
    /// a key-resolving search probe against the row, landing `on` only when
    /// the probe returns `Ok`. A missing key or dead endpoint can therefore
    /// never silently turn a backend on.
    pub(crate) fn on_backend_toggled(
        &mut self,
        index: usize,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.driver_do(
            |driver, fx| driver.toggle_web_backend(index, enabled, fx),
            cx,
        );
    }

    pub(crate) fn web_key_input(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.web_key_inputs
            .entry(id.to_owned())
            .or_insert_with(|| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx).placeholder(crate::i18n::t(
                        "Paste API key (Bearer is added)",
                        "粘贴 API 密钥（自动加 Bearer）",
                    ))
                });
                cx.subscribe_in(&input, window, |_, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
                .detach();
                input
            })
            .clone()
    }

    pub(crate) fn on_save_web_key(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.web_key_inputs.get(id).cloned() else {
            return;
        };
        let Some(api_key) = normalized_key_input(&input.read(cx).value()) else {
            return;
        };
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.web_key_replace.remove(id);
        self.dispatch(
            BridgeCommand::SaveProviderKey {
                provider_id: format!("web-{id}"),
                api_key,
            },
            cx,
        );
    }

    pub(crate) fn on_cancel_replace_web_key(&mut self, id: &str, cx: &mut Context<Self>) {
        self.web_key_replace.remove(id);
        self.web_key_inputs.remove(id);
        cx.notify();
    }

    pub(crate) fn web_key_replacing(&self, id: &str) -> bool {
        self.web_key_replace.contains(id)
    }

    /// Opens an empty field so a stored web key can be replaced. The stored
    /// secret is never copied into the field.
    pub(crate) fn on_replace_web_key(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.web_key_inputs.get(id).cloned() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.web_key_replace.insert(id.to_owned());
        cx.notify();
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

    pub(crate) fn mcp_key_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.mcp_key_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(crate::i18n::t("Paste API key", "粘贴 API 密钥"))
                })
            })
            .clone()
    }

    /// Adds the server the custom form currently describes: the pure builder
    /// next to [`McpForm`] validates the fields, this method dispatches.
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
        if id.is_empty() {
            self.apply_action(FrontendAction::Failed("fill id".to_owned()), cx);
            return;
        }
        if self
            .vm()
            .settings
            .as_ref()
            .is_some_and(|settings| settings.mcp_servers.iter().any(|server| server.id == id))
        {
            self.apply_action(
                FrontendAction::Failed(format!("an MCP server named '{id}' already exists")),
                cx,
            );
            return;
        }
        let server = match build_mcp_server(&id, &transport, &endpoint, &command, &env_line) {
            Ok(server) => server,
            Err(message) => {
                self.apply_action(FrontendAction::Failed(message), cx);
                return;
            }
        };
        self.apply_action(FrontendAction::SettingsMcpAdded(server), cx);
        if transport == "http"
            && let Some(api_key) = normalized_key_input(&api_key)
        {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: format!("mcp-{id}"),
                    api_key,
                },
                cx,
            );
        }
        form.update(cx, |form, cx| form.clear(window, cx));
        self.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
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
        let mut added = 0usize;
        let imported = match mycode_config::parse_mcp_import(&raw) {
            Ok(imported) => imported,
            Err(message) => {
                self.apply_action(FrontendAction::Failed(message), cx);
                return;
            }
        };
        let mut existing: Vec<String> = self
            .vm()
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .mcp_servers
                    .iter()
                    .map(|server| server.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut duplicates: Vec<String> = Vec::new();
        for row in imported {
            if existing.contains(&row.server.id) {
                duplicates.push(row.server.id.clone());
                continue;
            }
            let server_id = row.server.id.clone();
            existing.push(server_id.clone());
            let api_key = row.api_key.clone();
            self.apply_action(FrontendAction::SettingsMcpAdded(row.server), cx);
            added += 1;
            if let Some(api_key) = api_key.and_then(|key| normalized_key_input(&key)) {
                self.dispatch(
                    BridgeCommand::SaveProviderKey {
                        provider_id: format!("mcp-{server_id}"),
                        api_key,
                    },
                    cx,
                );
            }
        }
        input.update(cx, |state, cx| state.set_value("", window, cx));
        if added > 0 {
            self.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
        }
        if !duplicates.is_empty() {
            // One aggregated message: per-row failures overwrote each other,
            // so only the last duplicate name was ever reported.
            self.apply_action(
                FrontendAction::Failed(format!(
                    "MCP servers already exist: {}",
                    duplicates.join(", ")
                )),
                cx,
            );
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
        let Some(settings) = self.vm().settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        next.role_mut(role).enabled = enabled;
        self.apply_action(FrontendAction::SettingsSubagentsChanged(next), cx);
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
        let Some(settings) = self.vm().settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        let entry = next.role_mut(role);
        entry.thinking = thinking.filter(|level| level != "inherit" && level != "default");
        self.apply_action(FrontendAction::SettingsSubagentsChanged(next), cx);
    }

    pub(crate) fn on_set_subagent_route(
        &mut self,
        role: &str,
        provider: Option<String>,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.vm().settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        let entry = next.role_mut(role);
        if provider.as_deref() == Some("inherit") || model.as_deref() == Some("inherit") {
            entry.provider = None;
            entry.model = None;
        } else if let Some(provider) = provider {
            let model = model.or_else(|| {
                settings
                    .providers
                    .iter()
                    .find(|item| item.id == provider)
                    .and_then(|item| item.models.first().cloned())
            });
            match model {
                Some(model) => {
                    entry.provider = Some(provider);
                    entry.model = Some(model);
                }
                None => {
                    entry.provider = None;
                    entry.model = None;
                }
            }
        } else if let Some(model) = model {
            let fallback = self.vm().selected_provider.clone().or_else(|| {
                settings
                    .providers
                    .iter()
                    .find(|item| item.enabled)
                    .map(|item| item.id.clone())
            });
            match fallback {
                Some(provider) => {
                    entry.provider = Some(provider);
                    entry.model = Some(model);
                }
                None => {
                    entry.provider = None;
                    entry.model = None;
                }
            }
        }
        self.apply_action(FrontendAction::SettingsSubagentsChanged(next), cx);
    }

    /// Waits out the text-field debounce, then writes `settings.json`.
    /// Writes a pending text-field edit immediately. Blur and leaving
    /// Settings use this so the draft is not waiting on the debounce.
    pub(crate) fn flush_settings_text_save(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.flush_settings_text_save(fx), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::normalized_key_input;

    #[test]
    fn blank_key_inputs_are_refused_and_bearer_prefixes_are_stripped() {
        assert_eq!(normalized_key_input(""), None);
        assert_eq!(normalized_key_input("   \t\n  "), None);
        assert_eq!(normalized_key_input("Bearer   "), None);
        assert_eq!(
            normalized_key_input("  sk-test-123  "),
            Some("sk-test-123".to_owned())
        );
        assert_eq!(
            normalized_key_input("Bearer sk-test-123"),
            Some("sk-test-123".to_owned())
        );
    }
}
