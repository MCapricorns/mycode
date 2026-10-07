//! The self-update dialog: one modal that walks offer → download → install.
//! Discovery and download run automatically once an offer is resolved;
//! restarting into the install always waits for this dialog's confirm.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};

use crate::i18n::t;
use crate::ui::ellipsis;
use crate::view_model::UpdateState;
use crate::workspace::Workspace;

pub(super) fn render_update_dialog(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let vm = workspace.vm();
    let status_line = match &vm.update {
        UpdateState::Idle | UpdateState::UpToDate => {
            t("You're on the latest version.", "已是最新版本。").to_owned()
        }
        UpdateState::Checking => t("Checking for updates…", "正在检查更新…").to_owned(),
        UpdateState::Available { version, .. } => {
            format!("v{version} {}", t("is available.", "可用。"))
        }
        UpdateState::Downloading { version } => format!(
            "v{version} {}",
            t("is downloading and verifying…", "正在下载并校验…")
        ),
        UpdateState::Ready { version } => format!(
            "v{version} {}",
            t(
                "is staged. Restart to finish the install.",
                "已就绪。重启后完成安装。"
            )
        ),
        UpdateState::Failed(message) => format!(
            "{} {}",
            t("Update failed:", "更新失败:"),
            ellipsis(message, 200)
        ),
    };
    let detail = match &vm.update {
        UpdateState::Available { .. } => t(
            "The download starts on its own; installing waits for your confirm.",
            "下载会自动进行,安装需你确认。",
        ),
        UpdateState::Downloading { .. } => t(
            "The install prompt appears when the package is verified.",
            "校验通过后会弹出安装提示。",
        ),
        UpdateState::Ready { .. } => t(
            "Restarting closes every session. Uncommitted work stays on disk.",
            "重启会关闭所有会话。未提交的工作保留在磁盘上。",
        ),
        _ => "",
    };
    let checking = matches!(vm.update, UpdateState::Checking);
    let downloading = matches!(vm.update, UpdateState::Downloading { .. });
    type DialogAction = Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>;
    let primary = match &vm.update {
        UpdateState::Available { .. } => Some((
            IconName::Download,
            t("Download & install", "下载并安装").to_owned(),
            Box::new(|workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                workspace.on_download_update(cx);
            }) as DialogAction,
        )),
        UpdateState::Ready { .. } => Some((
            IconName::RefreshCw,
            t("Restart to install", "重启并安装").to_owned(),
            Box::new(|workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                workspace.on_install_update(cx);
            }) as DialogAction,
        )),
        UpdateState::Failed(_) => Some((
            IconName::RefreshCw,
            t("Retry", "重试").to_owned(),
            Box::new(|workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                workspace.on_check_update(true, cx);
            }) as DialogAction,
        )),
        _ => None,
    };
    div()
        .id("update-dialog-layer")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("update-dialog-backdrop")
                .absolute()
                .size_full()
                .bg(theme.background.opacity(0.5))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.apply_action(
                        crate::view_model::DesktopAction::UpdateDialogToggled(false),
                        cx,
                    );
                })),
        )
        .child(
            div()
                .id("update-dialog")
                .relative()
                .w(px(400.))
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded(px(12.))
                .border_1()
                .border_color(super::skin::glass_border(theme))
                .bg(theme.popover)
                .shadow_lg()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .child(t("Software update", "软件更新")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("v{}", mycode_app::current_version())),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(theme.foreground)
                        .child(status_line),
                )
                .when(!detail.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("update-dialog-later")
                                .label(t("Later", "稍后"))
                                .small()
                                .ghost()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.apply_action(
                                        crate::view_model::DesktopAction::UpdateDialogToggled(
                                            false,
                                        ),
                                        cx,
                                    );
                                })),
                        )
                        .when_some(primary, |this, (icon, label, action)| {
                            this.child(
                                Button::new("update-dialog-primary")
                                    .icon(icon)
                                    .label(label)
                                    .small()
                                    .primary()
                                    .disabled(checking || downloading)
                                    .on_click(cx.listener(move |workspace, _, _, cx| {
                                        action(workspace, cx);
                                    })),
                            )
                        }),
                ),
        )
        .into_any_element()
}
