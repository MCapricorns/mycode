//! The Models settings pages: the configured provider list, the models.dev
//! catalog picker with its preset form, and the custom-endpoint form.
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

use super::widgets::{dropdown_field, labeled_field, row_header, settings_card};
use crate::i18n::t;
use crate::ui::skin;
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

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
        crate::view_model::ModelsSubview::OpenDesign => {
            render_opendesign_page(workspace, window, cx)
        }
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
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
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
                        .gap_2()
                        .child(
                            Button::new("add-from-catalog")
                                .icon(IconName::Plus)
                                .label(t("Add from model library\u{2026}", "从模型库添加\u{2026}"))
                                .small()
                                .primary()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_models_subview(
                                        crate::view_model::ModelsSubview::Catalog,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new("add-opendesign")
                                .icon(IconName::Globe)
                                .label(t("OpenDesign\u{2026}", "OpenDesign\u{2026}"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_models_subview(
                                        crate::view_model::ModelsSubview::OpenDesign,
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

/// The catalog picker page: search + every provider from models.dev; picking
/// one opens the preset configuration form.
fn render_models_catalog_page(
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
        // OpenDesign has its own button and page; it is not a models.dev row.
        if provider.id == mycode_providers::catalog::opendesign::PROVIDER_ID {
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
    let header = super::subview_header(
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

/// The OpenDesign page: the live `/models` loader above the add form.
///
/// The form starts on the curated list. Loading replaces it with the models
/// the key can use; a blank key field uses the key already stored for the
/// configured OpenDesign provider.
fn render_opendesign_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    use mycode_providers::catalog::opendesign;

    let form = render_preset_form(workspace, opendesign::PROVIDER_ID, window, cx);
    let loading = workspace.vm().opendesign_loading;
    let configured = workspace.vm().settings.as_ref().is_some_and(|settings| {
        settings
            .providers
            .iter()
            .any(|provider| provider.id == opendesign::PROVIDER_ID)
    });
    let key_stored = workspace.vm().settings.as_ref().is_some_and(|settings| {
        settings
            .providers_with_keys
            .iter()
            .any(|id| id == opendesign::PROVIDER_ID)
    });
    let header = super::subview_header(
        t("Providers", "服务商"),
        "OpenDesign",
        Some(t(
            "Gateway at amr-link.open-design.ai, separate from the model library.",
            "amr-link.open-design.ai 网关，独立于模型库。",
        )),
        |workspace, cx| {
            workspace.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
        },
        cx,
    );
    let hint = if key_stored {
        t(
            "A key is stored. Load with the field blank to use it, or paste a new one.",
            "已保存密钥。输入框留空即用已保存的密钥加载，也可以粘贴新的。",
        )
    } else {
        t(
            "Paste your key in the form below, then load the models it can use.",
            "先在下方表单粘贴密钥，再加载它可用的模型。",
        )
    };
    let theme = cx.theme();
    div()
        .id("opendesign-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(
            settings_card(
                "opendesign-live",
                t("Live models", "实时模型"),
                Some(t(
                    "Until you load, the list shows the models measured against the gateway. \
                     Known models keep their measured limits and thinking levels.",
                    "加载前列出的是针对该网关实测过的模型。已知模型保留实测的上限和思考档位。",
                )),
                theme,
                vec![
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("opendesign-load")
                                .icon(IconName::RefreshCw)
                                .label(if loading {
                                    t("Loading\u{2026}", "加载中\u{2026}")
                                } else {
                                    t("Load models from gateway", "从网关加载模型")
                                })
                                .small()
                                .primary()
                                .disabled(loading)
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_load_opendesign_models(cx);
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(hint),
                        )
                        .into_any_element(),
                ],
            )
            .into_any_element(),
        )
        .when(configured, |this| {
            this.child(div().text_xs().text_color(theme.muted_foreground).child(t(
                "OpenDesign is configured. Adding again replaces its model list.",
                "OpenDesign 已配置。再次添加会替换它的模型列表。",
            )))
        })
        .child(form)
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
                        .apply_action(DesktopAction::SettingsProviderToggled(index, *checked), cx);
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
        .when(
            mycode_providers::catalog::uses_oauth_login(&preset.auth),
            |this| {
                let theme = cx.theme();
                let sign_in = workspace.vm().copilot_sign_in.clone();
                let error = workspace.vm().copilot_error.clone();
                let sign_label = match preset.id.as_str() {
                    "openai-codex" => t("Sign in with ChatGPT", "使用 ChatGPT 登录"),
                    _ => t("Sign in with GitHub", "使用 GitHub 登录"),
                };
                this.child(
                    div()
                        .id("preset-sign-in")
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
                                    .id("preset-sign-in-code")
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
                                    ),
                            )
                        })
                        .when(workspace.vm().copilot_sign_in.is_none(), |this| {
                            this.child(
                                Button::new("preset-sign-in-start")
                                    .label(sign_label)
                                    .small()
                                    .primary()
                                    .on_click(cx.listener(|workspace, _, _, cx| {
                                        workspace.on_start_oauth_sign_in(cx);
                                    })),
                            )
                        })
                        .child(
                            Button::new("preset-cancel-oauth")
                                .label(t("Close", "关闭"))
                                .small()
                                .ghost()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_close_preset(cx);
                                })),
                        ),
                )
            },
        )
        .when(
            preset.auth != mycode_providers::catalog::AUTH_DEVICE_CODE,
            |this| {
                this.child(
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
                        .gap_2()
                        .child(
                            Button::new("preset-confirm")
                                .icon(IconName::Check)
                                .label(t("Add provider", "添加服务商"))
                                .small()
                                .primary()
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
            },
        )
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

/// Validates and builds one custom provider from form inputs before
/// dispatching, so duplicate ids or invalid URLs cannot overwrite an existing
/// key or leave `settings` in an unsaveable state.
pub(crate) fn build_custom_provider(
    existing_ids: &[String],
    id: &str,
    kind: &str,
    base_url: &str,
    model: &str,
    context_limit: &str,
    max_output: &str,
) -> Result<mycode_config::ProviderSettings, String> {
    let id = id.trim();
    let kind = kind.trim();
    let base_url = base_url.trim();
    let model = model.trim();
    if id.is_empty() || base_url.is_empty() || model.is_empty() {
        return Err("fill id, base URL, and model".to_owned());
    }
    if existing_ids.iter().any(|existing| existing == id) {
        return Err(format!("a provider named '{id}' already exists"));
    }
    if existing_ids.len() >= mycode_config::MAX_PROVIDERS {
        return Err(format!(
            "at most {} providers may be configured",
            mycode_config::MAX_PROVIDERS
        ));
    }
    let (context_limit, max_output) = match (
        crate::workspace::parse_token_field(context_limit),
        crate::workspace::parse_token_field(max_output),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            return Err("context window and max output must be plain numbers".to_owned());
        }
    };
    let provider = mycode_config::ProviderSettings {
        id: id.to_owned(),
        kind: kind.to_owned(),
        base_url: base_url.to_owned(),
        models: vec![model.to_owned()],
        enabled: true,
        context_limit,
        max_output,
        hidden_models: Vec::new(),
    };
    let probe = mycode_config::AppSettings {
        providers: vec![provider.clone()],
        ..mycode_config::AppSettings::default()
    };
    probe
        .validate()
        .map_err(|error| format!("invalid provider: {}", error.summary()))?;
    Ok(provider)
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
        "provider-kind",
        t("Protocol", "协议"),
        Some(t("Wire protocol the endpoint speaks.", "端点采用的协议。")),
        &kind,
        &kind_options,
        kind_menu_open,
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
                                .primary()
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

#[cfg(test)]
mod tests;
