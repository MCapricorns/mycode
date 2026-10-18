//! The Models settings pages: the configured provider list and the
//! custom-endpoint form. The catalog picker lives in [`catalog`].
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::InputState;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{DropdownSpec, dropdown_field, labeled_field, row_header, settings_card};
use crate::i18n::t;
use crate::ui::skin;
use crate::view_model::FrontendAction;
use crate::workspace::Workspace;

mod catalog;

use self::catalog::{render_models_catalog_page, render_oauth_page};

pub(super) fn render_models_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match workspace.vm().models_subview {
        crate::view_model::ModelsSubview::List => {
            if workspace.vm().provider_detail.is_some() {
                super::provider_detail::render_provider_detail(workspace, window, cx)
            } else {
                render_models_list_page(workspace, window, cx)
            }
        }
        crate::view_model::ModelsSubview::Catalog => {
            render_models_catalog_page(workspace, window, cx)
        }
        crate::view_model::ModelsSubview::OAuth => render_oauth_page(workspace, window, cx),
        crate::view_model::ModelsSubview::Custom => {
            render_custom_provider_page(workspace, window, cx)
        }
    }
}

/// The Models landing page: default model, thinking, and provider cards.
fn render_models_list_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let menu_open = workspace.vm().model_menu_open;
    let picker = menu_open.then(|| {
        crate::ui::model_picker::render_model_picker(
            workspace,
            window,
            crate::ui::model_picker::ModelPickerTarget::Session,
            px(420.),
            cx,
        )
    });
    let model_label = crate::ui::model_picker::selected_model_label(workspace.vm());
    let levels = crate::view_model::selected_reasoning_levels(workspace.vm());
    let current_level = crate::view_model::selected_reasoning_level(workspace.vm());
    let thinking: Vec<(String, String)> = levels
        .iter()
        .map(|level| (level.clone(), crate::ui::chat::reasoning_row_label(level)))
        .collect();
    let catalog = workspace.vm().catalog.clone();
    let mut provider_rows: Vec<(usize, String, String, usize, bool, bool)> = Vec::new();
    {
        let Some(settings) = workspace.vm().settings.as_ref() else {
            return div().into_any_element();
        };
        for (index, provider) in settings.providers.iter().enumerate() {
            let name = catalog
                .as_ref()
                .map(|catalog| catalog.display_name(&provider.id))
                .unwrap_or_else(|| provider.id.clone());
            let host = provider
                .base_url
                .strip_prefix("https://")
                .unwrap_or(&provider.base_url)
                .split('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            let keyed = settings
                .providers_with_keys
                .iter()
                .any(|id| id == &provider.id);
            provider_rows.push((
                index,
                name,
                host,
                provider.models.len(),
                keyed,
                provider.enabled,
            ));
        }
    }
    let provider_row_elements: Vec<AnyElement> = provider_rows
        .into_iter()
        .map(|(index, name, host, models, keyed, enabled)| {
            provider_row(index, name, host, models, keyed, enabled, cx)
        })
        .collect();
    let theme = cx.theme();
    let providers_empty = provider_row_elements.is_empty();
    div()
        .id("models-list-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(
            settings_card(
                "default-model",
                t("Default model", "默认模型"),
                Some(t(
                    "The three most recent models are suggested at the top; pick any other from its provider.",
                    "顶部推荐最近使用的 3 个模型；其他模型从对应服务商里选。",
                )),
                theme,
                vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .id("default-model-toggle")
                                .h(px(36.))
                                .px_2()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .rounded(skin::radius_control())
                                .border_1()
                                .border_color(skin::glass_border(theme))
                                .bg(skin::frost(theme))
                                .cursor_pointer()
                                .hover(|this| this.bg(skin::frost_hover(theme)))
                                .on_click(cx.listener(|workspace, _, window, cx| {
                                    let open = !workspace.vm().model_menu_open;
                                    workspace.on_toggle_model_menu(open, window, cx);
                                }))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_sm()
                                        .child(model_label),
                                )
                                .child(
                                    Icon::new(IconName::ChevronDown)
                                        .xsmall()
                                        .text_color(theme.muted_foreground),
                                ),
                        )
                        .when_some(picker, |this, picker| this.child(picker))
                        .into_any_element(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(super::widgets::field_label(t("Thinking", "思考")))
                        .child(super::widgets::choice_chips(
                            "session-think",
                            &thinking,
                            &current_level,
                            |workspace, level, cx| workspace.on_select_reasoning(level, cx),
                            cx,
                        ))
                        .into_any_element(),
                ],
            )
            .into_any_element(),
        )
        .child(
            settings_card(
                "providers",
                t("Providers", "服务商"),
                Some(t(
                    "Connected means a key is stored. Open a provider to replace the key or sign in.",
                    "已连接表示密钥已保存。打开服务商可更换密钥或登录。",
                )),
                theme,
                vec![
                    div()
                        .when(providers_empty, |this| {
                            this.child(div().text_xs().text_color(theme.muted_foreground).child(t(
                                "No providers yet — add one below",
                                "还没有服务商 — 在下方添加",
                            )))
                        })
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(provider_row_elements)
                        .into_any_element(),
                    div()
                        .id("add-provider-row")
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_3()
                        .child(
                            Button::new("add-from-catalog")
                                .icon(IconName::Plus)
                                .label(t("Add from model library\u{2026}", "从模型库添加\u{2026}"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_models_subview(
                                        crate::view_model::ModelsSubview::Catalog,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new("oauth-sign-in")
                                .icon(IconName::KeyRound)
                                .label(t("OAuth sign-in\u{2026}", "OAuth 登录\u{2026}"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_models_subview(
                                        crate::view_model::ModelsSubview::OAuth,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new("add-custom-endpoint")
                                .icon(IconName::Terminal)
                                .label(t("Add custom endpoint\u{2026}", "添加自定义端点\u{2026}"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_models_subview(
                                        crate::view_model::ModelsSubview::Custom,
                                        cx,
                                    );
                                })),
                        )
                        .into_any_element(),
                ],
            )
            .into_any_element(),
        )
        .into_any_element()
}

fn provider_row(
    index: usize,
    name: String,
    host: String,
    models: usize,
    keyed: bool,
    enabled: bool,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    div()
        .id(format!("provider-row-{index}"))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .p_2()
        .rounded(skin::radius_control())
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::frost(theme))
        .child(
            div()
                .id(format!("provider-open-{index}"))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    let Some(id) = workspace
                        .vm()
                        .settings
                        .as_ref()
                        .and_then(|settings| settings.providers.get(index))
                        .map(|provider| provider.id.clone())
                    else {
                        return;
                    };
                    workspace.on_open_provider_detail(&id, cx);
                }))
                .child(row_header(
                    &name,
                    format!("{host} \u{b7} {models} {}", t("model(s)", "个模型")),
                    theme,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(if keyed {
                            theme.success
                        } else {
                            theme.muted_foreground
                        })
                        .child(crate::ui::lamp(if keyed {
                            theme.success
                        } else {
                            desk.faint
                        }))
                        .child(if keyed {
                            t("Connected", "已连接")
                        } else {
                            t("Not connected", "未连接")
                        }),
                )
                .child(
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground),
                ),
        )
        .child(
            Switch::new(format!("provider-toggle-{index}"))
                .checked(enabled)
                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                    workspace
                        .apply_action(FrontendAction::SettingsProviderToggled(index, *checked), cx);
                })),
        )
        .child(crate::ui::icon_button(
            format!("provider-remove-{index}"),
            IconName::Trash,
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_remove_provider(index, cx);
            }),
            cx,
        ))
        .into_any_element()
}

/// Inline custom-endpoint provider form state.
pub(crate) struct ProviderForm {
    /// Provider identity input.
    pub id: Entity<InputState>,
    /// Selected wire protocol (`anthropic-messages`, `openai-completions`,
    /// `openai-responses`).
    pub kind: String,
    /// Base URL input.
    pub base_url: Entity<InputState>,
    /// Default model input.
    pub model: Entity<InputState>,
    /// Optional context window override (tokens).
    pub context_limit: Entity<InputState>,
    /// Optional max output override (tokens).
    pub max_output: Entity<InputState>,
    /// API key input; stored in the secret store, never in settings.
    pub api_key: Entity<InputState>,
}

impl ProviderForm {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<Self> {
        let mut make = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let id = make(t("id, e.g. openai-main", "标识，例如 openai-main"));
        let base_url = make("https://api.example.com/v1");
        let model = make(t("model id", "模型 id"));
        let context_limit = make(t("optional, e.g. 200000", "可选，例如 200000"));
        let max_output = make(t("optional, e.g. 8192", "可选，例如 8192"));
        let api_key = make(t("api key (leave empty to skip)", "API 密钥（留空则跳过）"));
        cx.new(|_| Self {
            id,
            kind: "openai-completions".to_owned(),
            base_url,
            model,
            context_limit,
            max_output,
            api_key,
        })
    }

    pub(crate) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.id
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.kind = "openai-completions".to_owned();
        self.base_url
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.model
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.context_limit
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.max_output
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.api_key
            .update(cx, |state, cx| state.set_value("", window, cx));
    }
}

/// The custom-endpoint page: protocol dropdown, endpoint identity, and the
/// optional model parameter overrides.
fn render_custom_provider_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let form = workspace.provider_form(window, cx);
    let kind = form.read(cx).kind.clone();
    let kind_menu_open = workspace.vm().provider_kind_menu_open;
    let (context_input, max_output_input, base_url_input, model_input, id_input, api_key_input) = {
        let read = form.read(cx);
        (
            read.context_limit.clone(),
            read.max_output.clone(),
            read.base_url.clone(),
            read.model.clone(),
            read.id.clone(),
            read.api_key.clone(),
        )
    };
    let kind_options = [
        "anthropic-messages",
        "openai-completions",
        "openai-responses",
    ]
    .iter()
    .map(|kind| (*kind).to_owned())
    .collect::<Vec<_>>();
    let kind_field = dropdown_field(
        DropdownSpec {
            id: "provider-kind",
            label: t("Protocol", "协议"),
            description: Some(t("Wire protocol the endpoint speaks.", "端点采用的协议。")),
            current: &kind,
            options: &kind_options,
            open: kind_menu_open,
        },
        |workspace, open, cx| workspace.on_toggle_provider_kind_menu(open, cx),
        |workspace, kind, cx| workspace.on_select_provider_kind(kind, cx),
        cx,
    );
    let header = super::subview_header(
        t("Providers", "服务商"),
        t("Add custom endpoint", "添加自定义端点"),
        Some(t(
            "Any endpoint speaking one of the three wire protocols.",
            "任何支持这三种协议之一的端点。",
        )),
        |workspace, cx| {
            workspace.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
        },
        cx,
    );
    let theme = cx.theme();
    div()
        .id("custom-provider-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(
            settings_card(
                "custom-provider",
                t("Endpoint", "端点"),
                Some(t(
                    "The provider appears in the model picker as soon as it is added.",
                    "添加后该服务商立即出现在模型选择器中。",
                )),
                theme,
                vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_sm()
                        .child(labeled_field(t("id", "标识"), id_input, theme))
                        .child(kind_field)
                        .child(labeled_field(
                            t("base URL", "基础 URL"),
                            base_url_input,
                            theme,
                        ))
                        .child(labeled_field(
                            t("default model", "默认模型"),
                            model_input,
                            theme,
                        ))
                        .into_any_element(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_sm()
                        .child(labeled_field(
                            t("context window (optional)", "上下文窗口(可选)"),
                            context_input,
                            theme,
                        ))
                        .child(labeled_field(
                            t("max output tokens (optional)", "最大输出 token(可选)"),
                            max_output_input,
                            theme,
                        ))
                        .into_any_element(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_sm()
                        .child(labeled_field(
                            t(
                                "API key (stored in the secret vault)",
                                "API 密钥（保存在凭据库中）",
                            ),
                            api_key_input,
                            theme,
                        ))
                        .into_any_element(),
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            Button::new("provider-add")
                                .icon(IconName::Check)
                                .label(t("Add provider", "添加服务商"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, window, cx| {
                                    workspace.on_add_provider(window, cx);
                                })),
                        )
                        .into_any_element(),
                ],
            )
            .into_any_element(),
        )
        .into_any_element()
}
