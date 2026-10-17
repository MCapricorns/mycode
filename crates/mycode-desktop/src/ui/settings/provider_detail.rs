//! One configured provider: endpoint edit, key replacement, and device-code
//! sign-in. The endpoint uses the settings document's validation. A stored
//! API key is never written into the endpoint field.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::settings_card;
use crate::i18n::t;
use crate::ui::skin;
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

pub(super) fn render_provider_detail(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(provider_id) = workspace.vm().provider_detail.clone() else {
        return div().into_any_element();
    };
    let (provider, keyed, index) = {
        let Some(settings) = workspace.vm().settings.as_ref() else {
            return div().into_any_element();
        };
        let Some(index) = settings
            .providers
            .iter()
            .position(|provider| provider.id == provider_id)
        else {
            return missing_provider(cx);
        };
        let provider = settings.providers[index].clone();
        let keyed = settings
            .providers_with_keys
            .iter()
            .any(|id| id == &provider.id);
        (provider, keyed, index)
    };
    let catalog = workspace
        .vm()
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.provider(&provider.id))
        .cloned();
    let replacing = workspace.provider_key_replacing(&provider.id);
    let oauth = catalog.as_ref().is_some_and(|entry| {
        mycode_providers::catalog::uses_device_code_login(&entry.auth)
            && matches!(entry.id.as_str(), "github-copilot" | "openai-codex")
    });
    let show_key = catalog
        .as_ref()
        .is_none_or(|entry| entry.auth != mycode_providers::catalog::AUTH_DEVICE_CODE);
    let key_input = (show_key && (!keyed || replacing))
        .then(|| workspace.provider_key_input(&provider.id, window, cx));
    let endpoint_input =
        workspace.provider_endpoint_input(&provider.id, &provider.base_url, window, cx);
    let endpoint_pending = endpoint_input.read(cx).value().trim() != provider.base_url;
    let name = workspace
        .vm()
        .catalog
        .as_ref()
        .map(|catalog| catalog.display_name(&provider.id))
        .unwrap_or_else(|| provider.id.clone());
    let status = if keyed {
        t("Connected", "已连接")
    } else {
        t("Not connected", "未连接")
    };
    let sign_in = workspace.vm().copilot_sign_in.clone();
    let error = workspace.vm().copilot_error.clone();
    let enabled = provider.enabled;
    let header = super::subview_header(
        t("Providers", "服务商"),
        &name,
        Some(status),
        |workspace, cx| workspace.on_close_provider_detail(cx),
        cx,
    );
    let theme = cx.theme().clone();
    let id = provider.id.clone();
    let models = models_card(workspace, &provider, &theme, cx);
    div()
        .id("provider-detail")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(
            settings_card(
                "provider-detail",
                &name,
                Some(status),
                &theme,
                vec![
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(div().text_sm().child(t("Enabled", "启用")))
                        .child(
                            Switch::new(format!("provider-toggle-{index}"))
                                .checked(enabled)
                                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                                    workspace.apply_action(
                                        DesktopAction::SettingsProviderToggled(index, *checked),
                                        cx,
                                    );
                                })),
                        )
                        .into_any_element(),
                    endpoint_block(&id, endpoint_input, endpoint_pending, &theme, cx),
                    key_block(&id, keyed, replacing, show_key, key_input, &theme, cx),
                    if oauth {
                        oauth_block(&id, &provider.id, sign_in, error, &theme, cx)
                    } else {
                        div().into_any_element()
                    },
                ],
            )
            .into_any_element(),
        )
        .child(models)
        .into_any_element()
}

/// Every model this provider offers, each with a switch that shows or hides
/// it in the chat model picker.
fn models_card(
    workspace: &Workspace,
    provider: &mycode_config::ProviderSettings,
    theme: &gpui_kit::component::theme::Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let choices = crate::ui::model_picker::all_provider_model_choices(workspace.vm(), &provider.id);
    let shown = choices
        .iter()
        .filter(|choice| provider.shows_model(&choice.id))
        .count();
    let all_ids: Vec<String> = choices.iter().map(|choice| choice.id.clone()).collect();
    let bulk = |id: &'static str, label: &'static str, show: bool, disabled: bool| {
        let provider_id = provider.id.clone();
        let models = all_ids.clone();
        Button::new(id)
            .label(label)
            .small()
            .ghost()
            .disabled(disabled)
            .on_click(cx.listener(move |workspace, _, _, cx| {
                workspace.apply_action(
                    DesktopAction::SettingsModelsShown {
                        provider: provider_id.clone(),
                        models: models.clone(),
                        shown: show,
                    },
                    cx,
                );
            }))
    };
    let toolbar = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{} {shown} / {}",
                    t("Shown in chat:", "对话中显示:"),
                    choices.len()
                )),
        )
        .child(bulk(
            "provider-models-show-all",
            t("Show all", "全部显示"),
            true,
            shown == choices.len(),
        ))
        .child(bulk(
            "provider-models-hide-all",
            t("Hide all", "全部隐藏"),
            false,
            shown == 0,
        ))
        .into_any_element();
    let mut rows = vec![toolbar];
    rows.extend(choices.into_iter().enumerate().map(|(index, choice)| {
        let visible = provider.shows_model(&choice.id);
        let provider_id = provider.id.clone();
        let model = choice.id.clone();
        div()
            .id(("provider-model-row", index))
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .py_0p5()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .when(!visible, |this| this.text_color(theme.muted_foreground))
                            .child(choice.name.clone()),
                    )
                    .when(choice.name != choice.id, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .font_family(theme.mono_font_family.clone())
                                .child(choice.id.clone()),
                        )
                    }),
            )
            .child(
                Switch::new(("provider-model-shown", index))
                    .checked(visible)
                    .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                        workspace.apply_action(
                            DesktopAction::SettingsModelsShown {
                                provider: provider_id.clone(),
                                models: vec![model.clone()],
                                shown: *checked,
                            },
                            cx,
                        );
                    })),
            )
            .into_any_element()
    }));
    settings_card(
        "provider-models",
        t("Models", "模型"),
        Some(t(
            "Hidden models stay out of the chat model picker, its recent suggestions, and the command palette.",
            "隐藏的模型不会出现在对话的模型切换、最近使用推荐和命令面板中。",
        )),
        theme,
        rows,
    )
    .into_any_element()
}

fn missing_provider(cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(super::subview_header(
            t("Providers", "服务商"),
            t("Provider", "服务商"),
            None,
            |workspace, cx| workspace.on_close_provider_detail(cx),
            cx,
        ))
        .into_any_element()
}

fn endpoint_block(
    id: &str,
    input: gpui_kit::Entity<gpui_kit::component::input::InputState>,
    pending: bool,
    theme: &gpui_kit::component::theme::Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let id = id.to_owned();
    div()
        .id("provider-endpoint")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t("Endpoint", "端点")),
        )
        .child(
            div()
                .h(px(32.))
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .child(Input::new(&input)),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(t(
                    "https:// only. This edits the endpoint, not the key. Leaving the field writes it to settings.",
                    "只能是 https://。这里改的是端点，不是密钥。离开输入框后写入设置。",
                )),
        )
        .child(
            Button::new(format!("provider-endpoint-save-{id}"))
                .label(t("Save endpoint", "保存端点"))
                .small()
                .outline()
                .disabled(!pending)
                .on_click(cx.listener(move |workspace, _, window, cx| {
                    workspace.on_apply_provider_endpoint(&id, window, cx);
                })),
        )
        .into_any_element()
}

fn key_block(
    id: &str,
    keyed: bool,
    replacing: bool,
    show_key: bool,
    key_input: Option<gpui_kit::Entity<gpui_kit::component::input::InputState>>,
    theme: &gpui_kit::component::theme::Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    if !show_key {
        return div().into_any_element();
    }
    let id_owned = id.to_owned();
    div()
        .id("provider-key")
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
                    .id(format!("provider-key-lock-{id}"))
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
                        workspace.on_replace_provider_key(&id, window, cx);
                    }))
                    .child(Icon::new(IconName::Lock).small())
                    .child(div().text_sm().child(t("Replace key", "更换密钥"))),
            )
        })
        .when_some(key_input, |this, input| {
            let id = id_owned.clone();
            let cancel_id = id_owned.clone();
            let can_save =
                crate::workspace::normalized_key_input(&input.read(cx).value()).is_some();
            this.child(div().h(px(32.)).text_sm().child(Input::new(&input)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new(format!("provider-key-save-{id}"))
                                .label(t("Save key", "保存密钥"))
                                .small()
                                .outline()
                                .disabled(!can_save)
                                .on_click(cx.listener(move |workspace, _, window, cx| {
                                    workspace.on_save_provider_key(&id, window, cx);
                                })),
                        )
                        .when(keyed && replacing, |this| {
                            this.child(
                                Button::new(format!("provider-key-cancel-{cancel_id}"))
                                    .label(t("Cancel", "取消"))
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |workspace, _, _, cx| {
                                        workspace.on_cancel_replace_provider_key(&cancel_id, cx);
                                    })),
                            )
                        }),
                )
        })
        .into_any_element()
}

fn oauth_block(
    id: &str,
    catalog_id: &str,
    sign_in: Option<crate::view_model::CopilotSignIn>,
    error: Option<String>,
    theme: &gpui_kit::component::theme::Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let sign_label = match catalog_id {
        "openai-codex" => t("Sign in with ChatGPT", "使用 ChatGPT 登录"),
        _ => t("Sign in with GitHub", "使用 GitHub 登录"),
    };
    let waiting = sign_in.is_some();
    let id = id.to_owned();
    div()
        .id("provider-sign-in")
        .flex()
        .flex_col()
        .gap_2()
        .when_some(error, |this, message| {
            this.child(
                div()
                    .text_xs()
                    .p_2()
                    .rounded_md()
                    .bg(theme.danger.opacity(0.12))
                    .text_color(theme.danger)
                    .child(message),
            )
        })
        .when_some(sign_in, |this, sign_in| {
            this.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(skin::glass_border(theme))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} {} {}",
                                t("Open", "打开"),
                                sign_in.verification_uri,
                                t("and enter this code:", "并输入此验证码：")
                            )),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_family(theme.mono_font_family.clone())
                            .font_weight(crate::ui::skin::WEIGHT_STRONG)
                            .child(sign_in.user_code),
                    ),
            )
        })
        .when(!waiting, |this| {
            this.child(
                Button::new(format!("provider-sign-in-{id}"))
                    .label(sign_label)
                    .small()
                    .outline()
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        workspace.on_start_provider_oauth(&id, cx);
                    })),
            )
        })
        .into_any_element()
}
