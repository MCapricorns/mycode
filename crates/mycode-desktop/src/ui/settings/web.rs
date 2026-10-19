//! The Web settings page: one row per vendor backend that opens its detail
//! page. Vendor endpoints are fixed; a key lives in the vault, never in
//! settings. Retired `custom` rows are dropped when settings load, so every
//! row here is a vendor row.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{row_header, settings_card};
use crate::i18n::t;
use crate::ui::skin;
use crate::workspace::Workspace;

pub(super) fn render_web_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match workspace.vm().web_detail.clone() {
        Some(id) => render_web_detail(workspace, &id, window, cx),
        None => render_web_list(workspace, cx),
    }
}

/// Display name for a vendor kind; anything else shows its own id.
fn vendor_title(backend: &mycode_config::WebBackendSettings) -> String {
    match backend.kind.as_str() {
        "querit" => "Querit".to_owned(),
        "anysearch" => "AnySearch".to_owned(),
        "exa" => "Exa".to_owned(),
        _ => backend.id.clone(),
    }
}

/// Where the backend's key comes from, in the order the runtime looks.
#[derive(Clone, Copy)]
enum KeySource {
    /// The environment variable; the runtime reads it before the vault.
    Env(&'static str),
    /// A key pasted into the vault.
    Vault,
    /// No key, and the vendor answers anonymous requests.
    Anonymous,
    /// No key anywhere.
    Missing,
}

impl KeySource {
    fn of(backend: &mycode_config::WebBackendSettings, vault_keyed: bool) -> Self {
        let env = mycode_config::web_key_env(&backend.kind);
        if let Some(name) = env
            && std::env::var_os(name).is_some_and(|value| !value.is_empty())
        {
            Self::Env(name)
        } else if vault_keyed {
            Self::Vault
        } else if backend.kind == "anysearch" {
            Self::Anonymous
        } else {
            Self::Missing
        }
    }

    fn label(self) -> String {
        match self {
            Self::Env(name) => format!("{} {name}", t("Key from", "密钥来自")),
            Self::Vault => t("Connected", "已连接").to_owned(),
            Self::Anonymous => t("Anonymous", "匿名访问").to_owned(),
            Self::Missing => t("Not connected", "未连接").to_owned(),
        }
    }

    fn has_key(self) -> bool {
        matches!(self, Self::Env(_) | Self::Vault)
    }
}

fn vault_keyed(settings: &crate::view_model::SettingsState, backend_id: &str) -> bool {
    settings
        .providers_with_keys
        .iter()
        .any(|id| id.strip_prefix("web-") == Some(backend_id))
}

fn render_web_list(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(settings) = workspace.vm().settings.as_ref() else {
        return div().into_any_element();
    };
    let is_vendor = |backend: &mycode_config::WebBackendSettings| {
        mycode_config::web_key_env(&backend.kind).is_some() && backend.id == backend.kind
    };
    let cx: &Context<Workspace> = cx;
    let theme = cx.theme();
    let mut rows: Vec<AnyElement> = Vec::new();
    for (index, backend) in settings.web_backends.iter().enumerate() {
        if is_vendor(backend) {
            rows.push(vendor_row(
                index,
                vendor_title(backend),
                backend.endpoint.clone(),
                KeySource::of(backend, vault_keyed(settings, &backend.id)),
                backend.enabled,
                theme,
                cx,
            ));
        }
    }
    div()
        .id("web-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "web",
            t("Web search", "网页搜索"),
            Some(t(
                "Open a backend to paste its key. Keys live in the vault and show as a \
                 lock; QUERIT_API_KEY / ANYSEARCH_API_KEY / EXA_API_KEY also work. At \
                 most one backend is active, and enabling runs a live search probe \
                 first.",
                "打开后端粘贴密钥。密钥保存在凭据库中，显示为锁形标记；也支持环境变量 \
                 QUERIT_API_KEY / ANYSEARCH_API_KEY / EXA_API_KEY。最多启用一个后端，\
                 启用前会先做真实搜索探测。",
            )),
            theme,
            vec![
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(rows)
                    .into_any_element(),
            ],
        ))
        .into_any_element()
}

/// One vendor row: click opens its detail page; the switch toggles it in
/// place like the provider list does.
fn vendor_row(
    index: usize,
    title: String,
    endpoint: String,
    source: KeySource,
    enabled: bool,
    theme: &Theme,
    cx: &Context<Workspace>,
) -> AnyElement {
    let desk = crate::ui::desk::Desk::of(theme);
    let keyed = source.has_key();
    div()
        .id(format!("web-backend-{index}"))
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
                .id(format!("web-open-{index}"))
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
                        .and_then(|settings| settings.web_backends.get(index))
                        .map(|backend| backend.id.clone())
                    else {
                        return;
                    };
                    workspace.on_open_web_detail(&id, cx);
                }))
                .child(row_header(&title, endpoint, theme))
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
                        .child(source.label()),
                )
                .child(
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground),
                ),
        )
        .child(
            Switch::new(format!("backend-toggle-{index}"))
                .checked(enabled)
                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                    workspace.on_backend_toggled(index, *checked, cx);
                })),
        )
        .into_any_element()
}

/// One vendor's detail page: the enable switch, its fixed endpoint, and the
/// key field behind the vault lock.
fn render_web_detail(
    workspace: &mut Workspace,
    backend_id: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let (title, endpoint, enabled, env, anonymous, keyed, source, index) = {
        let Some(settings) = workspace.vm().settings.as_ref() else {
            return div().into_any_element();
        };
        let Some(index) = settings
            .web_backends
            .iter()
            .position(|backend| backend.id == backend_id)
        else {
            return missing_backend(cx);
        };
        let backend = &settings.web_backends[index];
        let keyed = vault_keyed(settings, backend_id);
        (
            vendor_title(backend),
            backend.endpoint.clone(),
            backend.enabled,
            mycode_config::web_key_env(&backend.kind),
            backend.kind == "anysearch",
            keyed,
            KeySource::of(backend, keyed),
            index,
        )
    };
    let replacing = workspace.web_key_replacing(backend_id);
    let key_input = (!keyed || replacing).then(|| workspace.web_key_input(backend_id, window, cx));
    let can_save_key = key_input.as_ref().is_some_and(|input| {
        crate::workspace::normalized_key_input(&input.read(cx).value()).is_some()
    });
    let status = source.label();
    let hint = match (env, anonymous) {
        (Some(env), true) => format!(
            "{} {env}. {}",
            t("The key may also come from", "密钥也可以来自"),
            t(
                "Anonymous access works without a key.",
                "没有密钥也能匿名访问。"
            ),
        ),
        (Some(env), false) => {
            format!(
                "{} {env}.",
                t("The key may also come from", "密钥也可以来自")
            )
        }
        _ => t("Authorization is sent as Bearer.", "鉴权以 Bearer 发送。").to_owned(),
    };
    let theme = cx.theme().clone();
    let id = backend_id.to_owned();
    div()
        .id("web-detail")
        .flex()
        .flex_col()
        .gap_3()
        .child(super::subview_header(
            t("Web search", "网页搜索"),
            &title,
            Some(&status),
            |workspace, cx| workspace.on_close_web_detail(cx),
            cx,
        ))
        .child(
            settings_card(
                "web-backend",
                &title,
                Some(hint.as_str()),
                &theme,
                vec![
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(div().text_sm().child(t("Enabled", "启用")))
                        .child(
                            Switch::new(format!("backend-toggle-{index}"))
                                .checked(enabled)
                                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                                    workspace.on_backend_toggled(index, *checked, cx);
                                })),
                        )
                        .into_any_element(),
                    div()
                        .id("web-endpoint")
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(t("Endpoint", "端点")),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_family(theme.mono_font_family.clone())
                                .child(endpoint),
                        )
                        .into_any_element(),
                    key_block(&id, keyed, replacing, key_input, can_save_key, &theme, cx),
                ],
            )
            .into_any_element(),
        )
        .into_any_element()
}

fn missing_backend(cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(super::subview_header(
            t("Web search", "网页搜索"),
            t("Backend", "后端"),
            None,
            |workspace, cx| workspace.on_close_web_detail(cx),
            cx,
        ))
        .into_any_element()
}

/// The vault-key block: a lock row that opens an empty field, or the field
/// with Save and Cancel. A stored key is never written into the field.
fn key_block(
    id: &str,
    keyed: bool,
    replacing: bool,
    input: Option<Entity<InputState>>,
    can_save: bool,
    theme: &Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let id_owned = id.to_owned();
    div()
        .id("web-key")
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_xs().text_color(theme.muted_foreground).child(t(
            "API key (stored in the secret vault)",
            "API 密钥（保存在凭据库中）",
        )))
        .when(keyed && !replacing, |this| {
            let id = id_owned.clone();
            this.child(
                div()
                    .id(format!("web-key-lock-{id}"))
                    .h(px(32.))
                    .px_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .rounded(skin::radius_control())
                    .border_1()
                    .border_color(skin::glass_border(theme))
                    .cursor_pointer()
                    .text_color(theme.success)
                    .hover(|this| this.bg(skin::frost_hover(theme)))
                    .on_click(cx.listener(move |workspace, _, window, cx| {
                        workspace.on_replace_web_key(&id, window, cx);
                    }))
                    .child(Icon::new(IconName::Lock).small())
                    .child(div().text_sm().child(t("Replace key", "更换密钥"))),
            )
        })
        .when_some(input, |this, input| {
            let id = id_owned.clone();
            let cancel_id = id_owned.clone();
            this.child(div().h(px(32.)).text_sm().child(Input::new(&input)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new(format!("web-key-save-{id}"))
                                .label(t("Save key", "保存密钥"))
                                .small()
                                .outline()
                                .disabled(!can_save)
                                .on_click(cx.listener(move |workspace, _, window, cx| {
                                    workspace.on_save_web_key(&id, window, cx);
                                })),
                        )
                        .when(keyed && replacing, |this| {
                            this.child(
                                Button::new(format!("web-key-cancel-{cancel_id}"))
                                    .label(t("Cancel", "取消"))
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |workspace, _, _, cx| {
                                        workspace.on_cancel_replace_web_key(&cancel_id, cx);
                                    })),
                            )
                        }),
                )
        })
        .into_any_element()
}
