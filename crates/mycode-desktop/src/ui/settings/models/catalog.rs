//! The provider catalog picker, the shared OAuth sign-in page, and the
//! preset form the catalog rows open.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::super::widgets::{row_header, settings_card};
use crate::i18n::t;
use crate::ui::skin;
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

/// The catalog picker page: search + every provider from models.dev; picking
/// one opens the preset configuration form.
pub(super) fn render_models_catalog_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(catalog) = workspace.vm().catalog.clone() else {
        return div().into_any_element();
    };
    // &mut Context work first: input entities and nested forms.
    let preset_search = workspace.preset_search_input(window, cx);
    let active_preset = workspace.vm().active_preset.clone();
    let preset_form_element = active_preset
        .as_ref()
        .map(|preset_id| render_preset_form(workspace, preset_id, window, cx))
        .unwrap_or_else(|| div().into_any_element());

    let preset_search_text = workspace.vm().preset_search.to_lowercase();
    let mut preset_rows: Vec<(String, String, String, usize)> = Vec::new();
    for provider in &catalog.providers {
        // Device-OAuth vendors sign in on the shared OAuth page; they are
        // not pasted-key rows of the library.
        if mycode_providers::catalog::uses_device_code_login(&provider.auth) {
            continue;
        }
        if !preset_search_text.is_empty()
            && !provider.name.to_lowercase().contains(&preset_search_text)
            && !provider.id.to_lowercase().contains(&preset_search_text)
        {
            continue;
        }
        preset_rows.push((
            provider.id.clone(),
            provider.name.clone(),
            provider.kind.clone(),
            provider.models.len(),
        ));
    }
    // Lazy rows via `uniform_list`: only the visible window of providers is
    // measured and painted per frame. The container gets a definite pixel
    // height (row count, capped) — the earlier virtualized attempt collapsed
    // because its container had no height bound at all.
    let preset_rows = std::rc::Rc::new(preset_rows);
    let preset_list_weak = cx.weak_entity();
    let list_height = px((preset_rows.len().clamp(1, 12) as f32) * PRESET_ROW_HEIGHT.as_f32() + 2.);
    let list_rows = preset_rows.clone();
    let presets_empty = preset_rows.is_empty();
    let preset_list = div()
        .id("preset-catalog-list")
        .w_full()
        .h(list_height)
        .overflow_hidden()
        .rounded(skin::radius_control())
        .border_1()
        .border_color(skin::glass_border(cx.theme()))
        .bg(skin::popover(cx.theme()))
        .when(presets_empty, |this| {
            let muted = cx.theme().muted_foreground;
            this.child(
                div()
                    .px_2()
                    .h(PRESET_ROW_HEIGHT)
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .child(t("No matching providers", "没有匹配的服务商")),
            )
        })
        .when(!presets_empty, |this| {
            this.child(
                gpui_kit::uniform_list(
                    "preset-catalog-rows",
                    preset_rows.len(),
                    move |range, _window, cx| {
                        let theme = cx.theme().clone();
                        range
                            .map(|index| {
                                let (id, name, kind, models) = &list_rows[index];
                                preset_row(
                                    id.clone(),
                                    name.clone(),
                                    kind.clone(),
                                    *models,
                                    &preset_list_weak,
                                    &theme,
                                )
                            })
                            .collect()
                    },
                )
                .h_full(),
            )
        });

    let has_preset = workspace.vm().active_preset.is_some();
    let header = super::super::subview_header(
        if has_preset {
            t("Model library", "模型库")
        } else {
            t("Providers", "服务商")
        },
        if has_preset {
            t("Configure provider", "配置服务商")
        } else {
            t("Add from model library", "从模型库添加")
        },
        if has_preset {
            None
        } else {
            Some(t(
                "190+ providers from models.dev",
                "来自 models.dev 的 190+ 服务商",
            ))
        },
        move |workspace, cx| {
            if has_preset {
                workspace.on_close_preset(cx);
            } else {
                workspace.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
            }
        },
        cx,
    );
    div()
        .id("models-catalog-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .when(workspace.vm().active_preset.is_none(), |this| {
            // No second card header here: the subview header above already
            // names the page, so the search field starts immediately.
            this.child(
                div()
                    .id("catalog-search")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().h(px(30.)).text_sm().child(Input::new(&preset_search)))
                    .child(preset_list)
                    .into_any_element(),
            )
        })
        .when(workspace.vm().active_preset.is_some(), |this| {
            this.child(preset_form_element)
        })
        .into_any_element()
}

/// The shared OAuth sign-in page: one card per device-flow provider
/// (GitHub Copilot, OpenAI Codex). Signing in binds the catalog row's
/// models; trimming them happens later on the provider's detail page.
pub(super) fn render_oauth_page(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let header = super::super::subview_header(
        t("Providers", "服务商"),
        t("OAuth sign-in", "OAuth 登录"),
        Some(t(
            "A device code signs in instead of a pasted API key.",
            "通过设备授权码登录，不需要粘贴 API 密钥。",
        )),
        |workspace, cx| {
            workspace.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
        },
        cx,
    );
    let error = workspace.vm().copilot_error.clone();
    let theme = cx.theme().clone();
    div()
        .id("oauth-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
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
        .child(oauth_card(
            workspace,
            mycode_providers::COPILOT_PROVIDER_ID,
            cx,
        ))
        .child(oauth_card(
            workspace,
            mycode_providers::OPENAI_CODEX_PROVIDER_ID,
            cx,
        ))
        .into_any_element()
}

/// One OAuth provider card on the shared sign-in page: status, model count,
/// the verification code while its flow runs, and the sign-in button.
fn oauth_card(
    workspace: &mut Workspace,
    provider_id: &'static str,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let (title, blurb, sign_label) = match provider_id {
        mycode_providers::OPENAI_CODEX_PROVIDER_ID => (
            "OpenAI Codex",
            t(
                "Signs in with your ChatGPT account over the OpenAI device flow.",
                "通过 OpenAI 设备授权流程使用你的 ChatGPT 账号登录。",
            ),
            t("Sign in with ChatGPT", "使用 ChatGPT 登录"),
        ),
        _ => (
            "GitHub Copilot",
            t(
                "Signs in with your GitHub account over the device flow.",
                "通过设备授权流程使用你的 GitHub 账号登录。",
            ),
            t("Sign in with GitHub", "使用 GitHub 登录"),
        ),
    };
    let settings = workspace.vm().settings.clone();
    let keyed = settings.as_ref().is_some_and(|settings| {
        settings
            .providers_with_keys
            .iter()
            .any(|id| id == provider_id)
    });
    let configured = settings.is_some_and(|settings| {
        settings
            .providers
            .iter()
            .any(|provider| provider.id == provider_id)
    });
    let status = if keyed {
        t("Connected", "已连接")
    } else {
        t("Not connected", "未连接")
    };
    let models = workspace
        .vm()
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.provider(provider_id))
        .map(|provider| provider.models.len())
        .unwrap_or_default();
    let sign_in = workspace
        .vm()
        .copilot_sign_in
        .clone()
        .filter(|sign_in| sign_in.provider_id == provider_id);
    // One device flow at a time: the idle card's button waits for the
    // running flow to finish or fail.
    let other_in_flight = workspace.vm().copilot_sign_in.is_some() && sign_in.is_none();
    let theme = cx.theme();
    let status_row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(if keyed {
                    theme.success
                } else {
                    theme.muted_foreground
                })
                .child(crate::ui::lamp(if keyed {
                    theme.success
                } else {
                    theme.muted_foreground
                }))
                .child(status),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("{models} {}", t("models", "个模型"))),
        )
        .when(configured && !keyed, |this| {
            this.child(div().text_xs().text_color(theme.muted_foreground).child(t(
                "configured — sign in to refresh",
                "已配置 — 重新登录以刷新",
            )))
        });
    let code_or_button = if let Some(sign_in) = sign_in {
        div()
            .id(format!("oauth-code-{provider_id}"))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(skin::glass_border(theme))
            .bg(skin::glass(theme))
            .child(div().text_xs().opacity(0.7).child(format!(
                "{} {} {}",
                t("Open", "打开"),
                sign_in.verification_uri,
                t("and enter this code:", "并输入此验证码：")
            )))
            .child(
                div()
                    .text_xl()
                    .font_family(theme.mono_font_family.clone())
                    .font_weight(crate::ui::skin::WEIGHT_STRONG)
                    .child(sign_in.user_code),
            )
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_row()
            .items_center()
            .child(
                Button::new(format!("oauth-start-{provider_id}"))
                    .label(sign_label)
                    .small()
                    .outline()
                    .disabled(other_in_flight)
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        workspace.on_start_oauth_sign_in(provider_id.to_owned(), cx);
                    })),
            )
            .into_any_element()
    };
    settings_card(
        provider_id,
        title,
        Some(blurb),
        theme,
        vec![status_row.into_any_element(), code_or_button],
    )
    .into_any_element()
}

/// One visible row of the virtualized preset catalog list. Fixed height via
/// `PRESET_ROW_HEIGHT` keeps `uniform_list` measurements uniform.
fn preset_row(
    id: String,
    name: String,
    kind: String,
    models: usize,
    weak: &gpui_kit::WeakEntity<Workspace>,
    theme: &Theme,
) -> AnyElement {
    let weak = weak.clone();
    div()
        .id(format!("preset-row-{id}"))
        .h(PRESET_ROW_HEIGHT)
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .rounded_md()
        // NOTE: no `overflow_hidden` on the row — inside a scroll container it
        // collapses the flex_1 name column to zero width on real windows
        // (headless layout tests do not reproduce this; verified on screen).
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .child(row_header(
            &name,
            format!("{id} · {kind} · {models} {}", t("model(s)", "个模型")),
            theme,
        ))
        .child(
            Button::new(format!("preset-add-{id}"))
                .icon(IconName::Plus)
                .label(t("Add", "添加"))
                .small()
                .outline()
                .on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_open_preset(&id, window, cx);
                    });
                }),
        )
        .into_any_element()
}

/// Fixed row height for the virtualized preset catalog list.
const PRESET_ROW_HEIGHT: gpui_kit::Pixels = px(48.);

/// One visible row of the virtualized preset model checklist.
fn preset_model_row(
    model: &str,
    selected: bool,
    weak: &gpui_kit::WeakEntity<Workspace>,
    theme: &Theme,
) -> AnyElement {
    let model_id = model.to_owned();
    let weak = weak.clone();
    div()
        .id(format!("preset-model-{model}"))
        .h(px(28.))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .rounded(skin::radius_control())
        .text_sm()
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click(move |_, _, cx| {
            let _ = weak.update(cx, |workspace, cx| {
                workspace.apply_action(DesktopAction::PresetModelToggled(model_id.clone()), cx);
            });
        })
        .child(model.to_owned())
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .text_color(theme.primary),
            )
        })
        .into_any_element()
}

/// The expanded preset form: model checklist plus the API key input.
fn render_preset_form(
    workspace: &mut Workspace,
    provider_id: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let key_input = workspace.preset_key_input(window, cx);
    let Some(catalog) = workspace.vm().catalog.clone() else {
        return div().into_any_element();
    };
    let Some(preset) = catalog.provider(provider_id) else {
        return div().into_any_element();
    };
    let model_search = workspace.preset_model_search_input(window, cx);
    let name = preset.name.clone();
    let query = workspace.vm().preset_model_query.to_lowercase();
    let models: Vec<(String, String)> = preset
        .models
        .iter()
        .filter(|model| {
            query.is_empty()
                || model.id.to_lowercase().contains(&query)
                || model.name.to_lowercase().contains(&query)
        })
        .map(|model| (model.id.clone(), model.name.clone()))
        .collect();
    let checked: Vec<String> = workspace.vm().preset_models.clone();
    let selected_count = preset
        .models
        .iter()
        .filter(|model| checked.contains(&model.id))
        .count();
    let selection_label = format!(
        "{} / {} {}",
        selected_count,
        preset.models.len(),
        t("models", "个模型")
    );
    let provider_id_owned = provider_id.to_owned();
    let theme = cx.theme();
    div()
        .id("preset-form")
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(skin::radius_card())
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::frost_card(theme))
        .child(
            div()
                .text_sm()
                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                .child(format!("{} {name}", t("Add", "添加"))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} — {}",
                            selection_label,
                            t(
                                "all are selected by default; uncheck what you do not need",
                                "默认全选；不需要的取消勾选即可",
                            )
                        )),
                )
                .child(div().h(px(32.)).text_sm().child(Input::new(&model_search)))
                .child({
                    let weak = cx.weak_entity();
                    let list_models = models.clone();
                    let list_checked = checked;
                    let shown = list_models.len().clamp(1, 8);
                    let list_height = px((shown as f32) * 28. + 2.);
                    let empty = models.is_empty();
                    div()
                        .id("preset-model-list")
                        .w_full()
                        .h(list_height)
                        .overflow_hidden()
                        .rounded(skin::radius_control())
                        .border_1()
                        .border_color(skin::glass_border(theme))
                        .bg(skin::popover(theme))
                        .when(empty, |this| {
                            this.child(
                                div()
                                    .px_2()
                                    .h(px(28.))
                                    .flex()
                                    .items_center()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(t("No matching models", "没有匹配的模型")),
                            )
                        })
                        .when(!empty, |this| {
                            this.child(
                                gpui_kit::uniform_list(
                                    "preset-model-rows",
                                    list_models.len(),
                                    move |range, _window, cx| {
                                        let theme = cx.theme().clone();
                                        range
                                            .map(|index| {
                                                let (model, _name) = &list_models[index];
                                                preset_model_row(
                                                    model,
                                                    list_checked.contains(model),
                                                    &weak,
                                                    &theme,
                                                )
                                            })
                                            .collect()
                                    },
                                )
                                .h_full(),
                            )
                        })
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(theme.muted_foreground).child(t(
                    "API key (stored in the secret vault)",
                    "API 密钥（保存在凭据库中）",
                )))
                .child(div().h(px(28.)).text_sm().child(Input::new(&key_input))),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_3()
                .child(
                    Button::new("preset-confirm")
                        .icon(IconName::Check)
                        .label(t("Add provider", "添加服务商"))
                        .small()
                        .outline()
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            let provider_id = provider_id_owned.clone();
                            workspace.on_add_preset(&provider_id, cx);
                        })),
                )
                .child(
                    Button::new("preset-cancel")
                        .label(t("Cancel", "取消"))
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_close_preset(cx);
                        })),
                ),
        )
        .into_any_element()
}
