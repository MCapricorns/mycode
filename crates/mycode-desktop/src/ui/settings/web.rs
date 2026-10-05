//! The Web settings page: vendor search backends with their key inputs, the
//! custom backend rows, and the add-backend form.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{labeled_field, row_header, settings_card};
use crate::i18n::t;
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
                                .opacity(0.5)
                                .whitespace_normal()
                                .child(t("No custom backends", "暂无自定义后端")),
                        )
                    })
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
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_color(theme.success)
                            .hover(|this| this.bg(theme.secondary))
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
                .opacity(0.6)
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
        .rounded(px(3.))
        .border_1()
        .border_color(theme.border)
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
                            .on_click({
                                let id = id.clone();
                                cx.listener(move |workspace, _, window, cx| {
                                    workspace.on_save_web_key(&id, window, cx);
                                })
                            }),
                    ),
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
        .rounded(px(3.))
        .border_1()
        .border_color(theme.border)
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
        let id = make("id, e.g. querit-main");
        let kind = make("querit | anysearch | custom");
        let endpoint = make("https://search.example.com");
        cx.new(|_| Self { id, kind, endpoint })
    }
}

fn render_backend_form(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let form = workspace.backend_form(window, cx);
    let theme: &Theme = cx.theme();
    div()
        .id("backend-form")
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
                .child(t("Add web search backend", "添加网页搜索后端")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .text_sm()
                .child(labeled_field("id", form.read(cx).id.clone()))
                .child(labeled_field("kind", form.read(cx).kind.clone()))
                .child(labeled_field("endpoint", form.read(cx).endpoint.clone())),
        )
        .child(
            Button::new("backend-add")
                .label(t("Add backend", "添加后端"))
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_add_backend(cx);
                })),
        )
        .into_any_element()
}
