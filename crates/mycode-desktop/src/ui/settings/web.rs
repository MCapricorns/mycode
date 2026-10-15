//! The Web settings page: vendor search backends with their key inputs, the
//! custom backend rows, and the add-backend form.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{labeled_field, row_header, settings_card};
use crate::i18n::t;
use crate::ui::skin;
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

pub(super) fn render_web_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match workspace.vm().web_subview {
        crate::view_model::WebSubview::List => render_web_list(workspace, window, cx),
        crate::view_model::WebSubview::Custom => render_web_custom_page(workspace, window, cx),
    }
}

fn render_web_list(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let keyed = settings.providers_with_keys.clone();
    let builtin_ids: Vec<String> = mycode_config::builtin_web_backends()
        .into_iter()
        .map(|backend| backend.id)
        .collect();
    let vendor_rows: Vec<AnyElement> = settings
        .web_backends
        .iter()
        .enumerate()
        .filter(|(_, backend)| builtin_ids.contains(&backend.id))
        .map(|(index, backend)| {
            let key_input = workspace.web_key_input(&backend.id, window, cx);
            vendor_backend_row(
                VendorBackend {
                    id: backend.id.clone(),
                    kind: backend.kind.clone(),
                    endpoint: backend.endpoint.clone(),
                    enabled: backend.enabled,
                    keyed: keyed.iter().any(|id| id == &format!("web-{}", backend.id)),
                    replacing: workspace.web_key_replacing(&backend.id),
                    index,
                },
                key_input,
                cx,
            )
        })
        .collect();
    let custom_rows: Vec<AnyElement> = settings
        .web_backends
        .iter()
        .enumerate()
        .filter(|(_, backend)| !builtin_ids.contains(&backend.id))
        .map(|(index, backend)| {
            backend_row(
                backend.id.clone(),
                backend.kind.clone(),
                backend.endpoint.clone(),
                backend.enabled,
                index,
                cx,
            )
        })
        .collect();
    let theme = cx.theme();
    let custom_empty = custom_rows.is_empty();
    div()
        .id("web-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "web",
            t("Web search", "网页搜索"),
            Some(t(
                "Querit and AnySearch are ready. A stored key stays in the vault and \
                 shows as a lock. Authorization is sent as Bearer — do not type Bearer \
                 yourself. QUERIT_API_KEY / ANYSEARCH_API_KEY also work.",
                "Querit 与 AnySearch 开箱可用。已保存的密钥保存在凭据库并显示为锁形标记。鉴权自动以 Bearer 发送 — 请不要自己输入 Bearer。也支持环境变量 QUERIT_API_KEY / ANYSEARCH_API_KEY。",
            )),
            theme,
            vendor_rows,
        ))
        .child(settings_card(
            "web-custom",
            t("Custom backends", "自定义后端"),
            Some(t(
                "A Querit-compatible endpoint (POST /v1/search, POST /v1/contents) \
                 or another AnySearch-compatible host.",
                "Querit 兼容端点(POST /v1/search、POST /v1/contents)或其他 AnySearch 兼容主机。",
            )),
            theme,
            vec![
                div()
                    .when(custom_empty, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .whitespace_normal()
                                .child(t("No custom backends", "暂无自定义后端")),
                        )
                    })
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(custom_rows)
                    .into_any_element(),
                div()
                    .id("add-web-backend-row")
                    .flex()
                    .flex_row()
                    .pt_1()
                    .child(
                        Button::new("add-custom-backend")
                            .icon(IconName::Plus)
                            .label(t("Add custom backend\u{2026}", "添加自定义后端\u{2026}"))
                            .small()
                            .primary()
                            .on_click(cx.listener(|workspace, _, _, cx| {
                                workspace
                                    .on_show_web_subview(crate::view_model::WebSubview::Custom, cx);
                            })),
                    )
                    .into_any_element(),
            ],
        ))
        .into_any_element()
}

fn render_web_custom_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let form = render_backend_form(workspace, window, cx);
    div()
        .id("web-custom-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(super::subview_header(
            t("Web search", "网页搜索"),
            t("Add custom backend", "添加自定义后端"),
            Some(t(
                "Querit-compatible or AnySearch-compatible host",
                "Querit 兼容或 AnySearch 兼容的主机",
            )),
            |workspace, cx| {
                workspace.on_show_web_subview(crate::view_model::WebSubview::List, cx);
            },
            cx,
        ))
        .child(form)
        .into_any_element()
}

struct VendorBackend {
    id: String,
    kind: String,
    endpoint: String,
    enabled: bool,
    keyed: bool,
    replacing: bool,
    index: usize,
}

fn vendor_backend_row(
    backend: VendorBackend,
    key_input: Entity<InputState>,
    cx: &Context<Workspace>,
) -> AnyElement {
    let VendorBackend {
        id,
        kind,
        endpoint,
        enabled,
        keyed,
        replacing,
        index,
    } = backend;
    let show_field = !keyed || replacing;
    let can_save_key =
        crate::workspace::normalized_key_input(&key_input.read(cx).value()).is_some();
    let theme = cx.theme();
    let title = match kind.as_str() {
        "querit" => "Querit",
        "anysearch" => "AnySearch",
        other => other,
    };
    // The vendor header adds the key marker beside the title, so it keeps
    // its own column instead of the plain two-line row_header.
    let header = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(title.to_owned()),
                )
                .when(keyed && !replacing, |this| {
                    let id = id.clone();
                    this.child(
                        div()
                            .id(format!("web-key-lock-{id}"))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(22.))
                            .rounded(skin::radius_xs())
                            .cursor_pointer()
                            .text_color(theme.success)
                            .hover(|this| this.bg(skin::frost_hover(theme)))
                            .child(Icon::new(IconName::Lock).small())
                            .on_click(cx.listener(move |workspace, _, window, cx| {
                                workspace.on_replace_web_key(&id, window, cx);
                            })),
                    )
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(endpoint),
        );
    div()
        .id(format!("vendor-backend-{id}"))
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(skin::radius_control())
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::frost(theme))
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(header)
                .child(
                    Switch::new(format!("backend-toggle-{id}"))
                        .checked(enabled)
                        .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                            workspace.apply_action(
                                DesktopAction::SettingsBackendToggled(index, *checked),
                                cx,
                            );
                        })),
                ),
        )
        .when(show_field, |this| {
            this.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(28.))
                            .child(Input::new(&key_input)),
                    )
                    .child(
                        Button::new(format!("web-key-save-{id}"))
                            .label(t("Save key", "保存密钥"))
                            .small()
                            .outline()
                            .disabled(!can_save_key)
                            .on_click({
                                let id = id.clone();
                                cx.listener(move |workspace, _, window, cx| {
                                    workspace.on_save_web_key(&id, window, cx);
                                })
                            }),
                    )
                    .when(keyed && replacing, |this| {
                        let cancel_id = id.clone();
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

fn backend_row(
    id: String,
    kind: String,
    endpoint: String,
    enabled: bool,
    index: usize,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id(format!("backend-row-{id}"))
        .w_full()
        .flex()
        .flex_row()
        .items_start()
        .gap_3()
        .p_2()
        .rounded(skin::radius_control())
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::frost(theme))
        .child(row_header(&id, format!("{kind} \u{b7} {endpoint}")))
        .child(
            Switch::new(format!("backend-toggle-{id}"))
                .checked(enabled)
                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                    workspace
                        .apply_action(DesktopAction::SettingsBackendToggled(index, *checked), cx);
                })),
        )
        .child(crate::ui::icon_button(
            format!("backend-remove-{id}"),
            IconName::Trash,
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_remove_backend(index, cx);
            }),
            cx,
        ))
        .into_any_element()
}

/// Inline add-backend form state.
pub(crate) struct BackendForm {
    /// Backend identity input.
    pub id: Entity<InputState>,
    /// Backend kind input (`querit` or `custom`).
    pub kind: Entity<InputState>,
    /// HTTPS endpoint input.
    pub endpoint: Entity<InputState>,
}

impl BackendForm {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<Self> {
        let mut make = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let id = make(t("id, e.g. querit-main", "标识，例如 querit-main"));
        let kind = make("querit | anysearch | custom");
        let endpoint = make("https://search.example.com");
        cx.new(|_| Self { id, kind, endpoint })
    }

    pub(crate) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.id
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.kind
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.endpoint
            .update(cx, |state, cx| state.set_value("", window, cx));
    }
}

/// Validates and builds one custom search backend from form inputs before
/// dispatching, so duplicate ids or bad URLs cannot enter the editor state.
pub(crate) fn build_custom_backend(
    existing_ids: &[String],
    id: &str,
    kind: &str,
    endpoint: &str,
) -> Result<mycode_config::WebBackendSettings, String> {
    let id = id.trim();
    let kind = kind.trim();
    let endpoint = endpoint.trim();
    if id.is_empty() || kind.is_empty() || endpoint.is_empty() {
        return Err("fill id, kind, and endpoint".to_owned());
    }
    if existing_ids.iter().any(|existing| existing == id) {
        return Err(format!("a web backend named '{id}' already exists"));
    }
    if existing_ids.len() >= mycode_config::MAX_WEB_BACKENDS {
        return Err(format!(
            "at most {} web backends may be configured",
            mycode_config::MAX_WEB_BACKENDS
        ));
    }
    let backend = mycode_config::WebBackendSettings {
        id: id.to_owned(),
        kind: kind.to_owned(),
        endpoint: endpoint.to_owned(),
        enabled: false,
    };
    let mut probe = mycode_config::AppSettings::default();
    probe.web.backends = vec![backend.clone()];
    probe
        .validate()
        .map_err(|error| format!("invalid backend: {}", error.summary()))?;
    Ok(backend)
}

fn render_backend_form(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let form = workspace.backend_form(window, cx);
    let (id_input, kind_input, endpoint_input) = {
        let read = form.read(cx);
        (read.id.clone(), read.kind.clone(), read.endpoint.clone())
    };
    let theme: &Theme = cx.theme();
    settings_card(
        "backend-form",
        t("Add web search backend", "添加网页搜索后端"),
        Some(t(
            "Use https:// and a unique id.",
            "使用 https:// 与唯一标识。",
        )),
        theme,
        vec![
            div()
                .flex()
                .flex_col()
                .gap_2()
                .text_sm()
                .child(labeled_field(t("id", "标识"), id_input))
                .child(labeled_field(t("kind", "类型"), kind_input))
                .child(labeled_field(t("endpoint", "端点"), endpoint_input))
                .into_any_element(),
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    Button::new("backend-add")
                        .icon(IconName::Check)
                        .label(t("Add backend", "添加后端"))
                        .small()
                        .primary()
                        .on_click(cx.listener(|workspace, _, window, cx| {
                            workspace.on_add_backend(window, cx);
                        })),
                )
                .into_any_element(),
        ],
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::build_custom_backend;

    #[test]
    fn custom_backend_builder_rejects_duplicate_ids_and_invalid_fields() {
        let existing = vec!["querit".to_owned(), "anysearch".to_owned()];
        let duplicate = build_custom_backend(
            &existing,
            " querit ",
            "querit",
            "https://search.example.com",
        )
        .expect_err("duplicate backend id must be refused");
        assert!(duplicate.contains("already exists"), "{duplicate}");

        let bad_kind = build_custom_backend(
            &existing,
            "search-main",
            "google",
            "https://search.example.com",
        )
        .expect_err("unknown backend kind must be refused");
        assert!(bad_kind.contains("invalid backend"), "{bad_kind}");

        let bad_url = build_custom_backend(
            &existing,
            "search-main",
            "custom",
            "http://search.example.com",
        )
        .expect_err("non-https endpoint must be refused");
        assert!(bad_url.contains("https://"), "{bad_url}");

        let valid = build_custom_backend(
            &existing,
            " search-main ",
            " custom ",
            " https://search.example.com ",
        )
        .expect("valid custom backend");
        assert_eq!(valid.id, "search-main");
        assert_eq!(valid.kind, "custom");
        assert_eq!(valid.endpoint, "https://search.example.com");
        assert!(!valid.enabled);
    }
}
