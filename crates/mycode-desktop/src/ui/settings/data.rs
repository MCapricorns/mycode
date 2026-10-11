//! The Data settings page: export and import of the product configuration.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div};

use super::widgets::settings_card;
use crate::i18n::t;
use crate::workspace::Workspace;

pub(super) fn render_data_section(
    _workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> AnyElement {
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
        .into_any_element();
    let scope_row = div()
        .text_xs()
        .opacity(0.55)
        .child(t(
            "Settings, UI state, and API keys \u{2014} conversations are never included.",
            "设置、界面状态与 API 密钥 — 绝不包含对话。",
        ))
        .into_any_element();
    let warning_row = div()
        .text_xs()
        .opacity(0.55)
        .child(t(
            "The export file stores API keys as plaintext JSON; keep it somewhere safe.",
            "导出文件以明文 JSON 存储 API 密钥，请妥善保管。",
        ))
        .into_any_element();
    settings_card(
        "data",
        t("Data", "数据"),
        Some(t(
            "Moving your configuration between machines.",
            "在不同机器之间迁移配置。",
        )),
        theme,
        vec![transfer_row, scope_row, warning_row],
    )
    .into_any_element()
}
