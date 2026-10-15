//! The Data settings page: export and import of the product configuration.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div};

use super::widgets::settings_card;
use crate::i18n::t;
use crate::workspace::Workspace;

/// Asks before an import that adds, changes, or removes MCP server commands.
fn import_confirm_row(bundle: &std::path::Path, cx: &mut Context<Workspace>) -> AnyElement {
    let theme = cx.theme();
    let name = bundle.file_name().map_or_else(
        || bundle.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    div()
        .id("data-import-confirm")
        .flex()
        .flex_col()
        .gap_2()
        .px_2()
        .py_2()
        .rounded(crate::ui::skin::radius_control())
        .bg(theme.warning.opacity(0.18))
        .text_color(theme.warning_foreground)
        .child(div().text_xs().whitespace_normal().child(format!(
            "{name}: {}",
            t(
                "this bundle adds, changes, or removes MCP server commands. They start programs on this computer when a chat runs. Import only a file you trust.",
                "此文件会新增、修改或删除 MCP 服务器命令，聊天运行时它们会在本机启动程序。只导入你信任的文件。",
            )
        )))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    Button::new("data-import-confirm-yes")
                        .label(t("Import anyway", "仍然导入"))
                        .small()
                        .primary()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_confirm_import(cx);
                        })),
                )
                .child(
                    Button::new("data-import-confirm-no")
                        .label(t("Cancel", "取消"))
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_cancel_import(cx);
                        })),
                ),
        )
        .into_any_element()
}

pub(super) fn render_data_section(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let confirm_row = workspace
        .import_confirm()
        .map(std::path::Path::to_path_buf)
        .map(|bundle| import_confirm_row(&bundle, cx));
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
    let mut rows = vec![transfer_row];
    rows.extend(confirm_row);
    rows.extend([scope_row, warning_row]);
    settings_card(
        "data",
        t("Data", "数据"),
        Some(t(
            "Moving your configuration between machines.",
            "在不同机器之间迁移配置。",
        )),
        theme,
        rows,
    )
    .into_any_element()
}
