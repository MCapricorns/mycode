//! The About settings page: version, self-update controls, and the provider
//! catalog date. Every transient notice is a toast; nothing status-like is
//! painted here.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement, Styled, div};

use super::widgets::{settings_card, settings_row};
use crate::i18n::t;
use crate::view_model::UpdateState;
use crate::workspace::Workspace;

pub(super) fn render_about_section(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let vm = workspace.vm();
    let current = mycode_app::current_version().to_owned();
    let catalog_line = match vm.catalog_fetched_at {
        0 => t("built-in providers", "内置服务商").to_owned(),
        seconds => {
            let date = catalog_date(seconds);
            format!(
                "{} · {} {date}",
                t("cloud catalog", "云端目录"),
                t("fetched", "获取于")
            )
        }
    };
    let auto_update = vm.auto_update;
    let checking = matches!(vm.update, UpdateState::Checking);
    let downloading = matches!(vm.update, UpdateState::Downloading { .. });
    // Status text is a toast; this page only carries the action that state
    // needs (download a staged offer, restart to install, retry a check).
    let action: Option<AnyElement> = match &vm.update {
        UpdateState::Available { .. } => Some(
            Button::new("update-download")
                .icon(IconName::Download)
                .label(t("Download & install", "下载并安装"))
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_download_update(cx);
                }))
                .into_any_element(),
        ),
        UpdateState::Ready { .. } => Some(
            Button::new("update-restart")
                .icon(IconName::RefreshCw)
                .label(t("Restart to install", "重启并安装"))
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_install_update(cx);
                }))
                .into_any_element(),
        ),
        _ => None,
    };
    let rows = vec![
        settings_row(
            "version",
            t("Current version", "当前版本"),
            None,
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!("v{current}"))
                .into_any_element(),
            cx.theme(),
        ),
        settings_row(
            "author",
            t("Author", "作者"),
            None,
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("MaMy")
                .into_any_element(),
            cx.theme(),
        ),
        settings_row(
            "thanks",
            t("Thanks", "致谢"),
            Some(t("People who built MYCode.", "参与构建 MYCode 的人。")),
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("MaMy, YangChengxxyy, iKunCai")
                .into_any_element(),
            cx.theme(),
        ),
        settings_row(
            "auto-update",
            t("Automatic checks", "自动检查"),
            Some(t(
                "Check GitHub for a newer release once a day.",
                "每天在 GitHub 检查一次新版本。",
            )),
            Switch::new("update-auto-toggle")
                .checked(auto_update)
                .on_click(cx.listener(|workspace, checked: &bool, _, cx| {
                    workspace.on_toggle_auto_update(*checked, cx);
                }))
                .into_any_element(),
            cx.theme(),
        ),
        settings_row(
            "updates",
            t("Updates", "更新"),
            None,
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .when_some(action, |this, action| this.child(action))
                .child(
                    Button::new("check-update")
                        .icon(IconName::RefreshCw)
                        .label(if checking {
                            t("Checking…", "检查中…")
                        } else if downloading {
                            t("Downloading…", "下载中…")
                        } else {
                            t("Check for updates", "检查更新")
                        })
                        .small()
                        .ghost()
                        .disabled(checking)
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_check_update(true, cx);
                        })),
                )
                .into_any_element(),
            cx.theme(),
        ),
        settings_row(
            "catalog",
            t("Provider catalog", "服务商目录"),
            None,
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(catalog_line),
                )
                .child(
                    Button::new("catalog-refresh")
                        .icon(IconName::RefreshCw)
                        .label(t("Refresh", "刷新"))
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_refresh_catalog(cx);
                        })),
                )
                .into_any_element(),
            cx.theme(),
        ),
    ];
    let theme = cx.theme();
    settings_card(
        "about",
        t("About", "关于"),
        Some(t(
            "MYCode updates itself from GitHub releases.",
            "MYCode 通过 GitHub release 自更新。",
        )),
        theme,
        rows,
    )
    .into_any_element()
}

/// The local calendar date of a unix timestamp, `YYYY-MM-DD`.
fn catalog_date(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|utc| {
            utc.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}
