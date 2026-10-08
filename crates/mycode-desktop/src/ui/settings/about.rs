//! The About settings page: version, self-update status, and the provider
//! catalog snapshot date.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
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
        0 => t("bundled snapshot", "内置快照").to_owned(),
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
    let status: (String, Option<AnyElement>) = match &vm.update {
        UpdateState::Idle => (
            t("Update checks run at startup.", "启动时会检查更新。").to_owned(),
            None,
        ),
        UpdateState::Checking => (t("Checking for updates…", "正在检查更新…").to_owned(), None),
        UpdateState::UpToDate => (
            format!("v{current} {}", t("is the latest version.", "是最新版本。")),
            Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().success)
                    .child(t("up to date", "已是最新"))
                    .into_any_element(),
            ),
        ),
        UpdateState::Available { version, .. } => (
            format!("v{version} {}", t("is available.", "可用。")),
            Some(
                Button::new("update-download")
                    .icon(IconName::Download)
                    .label(t("Download & install", "下载并安装"))
                    .small()
                    .primary()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_download_update(cx);
                    }))
                    .into_any_element(),
            ),
        ),
        UpdateState::Downloading { .. } => (
            t("Downloading and verifying…", "正在下载并校验…").to_owned(),
            None,
        ),
        UpdateState::Ready { version } => (
            format!("v{version} {}", t("is staged.", "已就绪。")),
            Some(
                Button::new("update-restart")
                    .icon(IconName::RefreshCw)
                    .label(t("Restart to install", "重启并安装"))
                    .small()
                    .primary()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_install_update(cx);
                    }))
                    .into_any_element(),
            ),
        ),
        UpdateState::Failed(_) => {
            // The failure text is the bottom-right toast. A red dump of the
            // transport error overflows this row.
            (
                t("Last update attempt failed.", "上次更新失败。").to_owned(),
                None,
            )
        }
    };
    let rows = vec![
        settings_row(
            "version",
            t("Current version", "当前版本"),
            None,
            div()
                .text_sm()
                .opacity(0.8)
                .child(format!("v{current}"))
                .into_any_element(),
        ),
        settings_row(
            "author",
            t("Author", "作者"),
            None,
            div()
                .text_sm()
                .opacity(0.8)
                .child("MaMy")
                .into_any_element(),
        ),
        settings_row(
            "thanks",
            t("Thanks", "致谢"),
            Some(t("People who built MYCode.", "参与构建 MYCode 的人。")),
            div()
                .text_sm()
                .opacity(0.8)
                .child("MaMy, YangChengxxyy, iKunCai")
                .into_any_element(),
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
        ),
        settings_row(
            "update-status",
            t("Status", "状态"),
            None,
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .text_xs()
                        .opacity(0.7)
                        .whitespace_normal()
                        .child(status.0),
                )
                .child(
                    Button::new("check-update")
                        .icon(IconName::RefreshCw)
                        .label(if checking {
                            t("Checking…", "检查中…")
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
        ),
        status
            .1
            .map(|node| {
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(node)
                    .into_any_element()
            })
            .unwrap_or_else(|| div().into_any_element()),
        settings_row(
            "catalog",
            t("Provider catalog", "服务商目录"),
            None,
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().text_xs().opacity(0.6).child(catalog_line))
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

fn catalog_date(seconds: u64) -> String {
    // Bounded ISO-ish date from unix seconds without a chrono dependency.
    let days = seconds / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
