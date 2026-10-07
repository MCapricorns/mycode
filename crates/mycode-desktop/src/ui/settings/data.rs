//! The Data settings page: durable usage records and export/import.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div};

use super::widgets::{settings_card, settings_row};
use crate::i18n::t;
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

pub(super) fn render_data_section(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let usage_enabled = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.usage_enabled)
        .unwrap_or(true);
    let usage_row = settings_row(
        "usage",
        t("Durable usage records", "持久化用量记录"),
        Some(t(
            "Write a Usage event per completed turn, aggregated in the Model panel.",
            "每完成一轮后写入一条用量记录,并在模型面板中汇总。",
        )),
        Switch::new("settings-usage-toggle")
            .checked(usage_enabled)
            .on_click(cx.listener(|workspace, checked: &bool, _, cx| {
                workspace.apply_action(DesktopAction::SettingsUsageToggled(*checked), cx);
            }))
            .into_any_element(),
    );
    let theme = cx.theme();
    let transfer_row = div()
        .id("data-transfer")
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .pt_1()
        .child(
            Button::new("data-export")
                .icon(IconName::Download)
                .label(t("Export data\u{2026}", "导出数据\u{2026}"))
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_export_data(cx);
                })),
        )
        .child(
            Button::new("data-import")
                .icon(IconName::Upload)
                .label(t("Import data\u{2026}", "导入数据\u{2026}"))
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_import_data(cx);
                })),
        )
        .child(div().text_xs().opacity(0.55).child(t(
            "Settings and sessions \u{2014} API keys stay on this machine.",
            "设置与会话 — API 密钥只保留在本机。",
        )))
        .into_any_element();
    settings_card(
        "data",
        t("Data", "数据"),
        Some(t(
            "Usage records and moving your configuration between machines.",
            "用量记录,以及在不同机器之间迁移配置。",
        )),
        theme,
        vec![usage_row, transfer_row],
    )
    .into_any_element()
}
