//! The Shell settings page: which program tool scripts launch with.
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, Window, div,
};

use super::widgets::{dropdown_field, settings_card, settings_row};
use crate::i18n::t;
use crate::workspace::Workspace;

pub(super) fn render_shell_section(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let shell = workspace
        .vm()
        .settings
        .as_ref()
        .and_then(|settings| settings.tools.shell.clone());
    let shell_kind = shell
        .as_ref()
        .map(|item| item.kind.clone())
        .unwrap_or_else(|| "auto".to_owned());
    let shell_program = shell
        .as_ref()
        .map(|item| item.program.clone())
        .unwrap_or_default();
    let shell_source = shell
        .as_ref()
        .map(|item| {
            if item.source.is_empty() {
                "auto".to_owned()
            } else {
                item.source.clone()
            }
        })
        .unwrap_or_else(|| "auto".to_owned());
    let shell_status = if shell_program.is_empty() {
        t(
            "No shell found. Detect one or browse to pwsh or Git bash.",
            "未找到 shell。可自动检测,或浏览选择 pwsh 或 Git bash。",
        )
        .to_owned()
    } else {
        format!("{shell_kind} · {shell_program} ({shell_source})")
    };
    let shell_kind_open = workspace.vm().shell_kind_menu_open;
    let shell_options = ["pwsh", "bash"]
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect::<Vec<_>>();
    let theme = cx.theme();
    div()
        .id("shell-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "shell",
            t("Shell", "Shell"),
            Some(t(
                "First launch detects pwsh, then Git bash. Override it here if \
                 detection misses your install.",
                "首次启动会依次探测 pwsh、Git bash。如果检测不到,可在这里手动指定。",
            )),
            theme,
            vec![
                settings_row(
                    "shell-current",
                    t("Current program", "当前程序"),
                    Some(shell_status.as_str()),
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .child(
                            Button::new("shell-detect")
                                .label(t("Detect", "检测"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_detect_shell(cx);
                                })),
                        )
                        .child(
                            Button::new("shell-browse")
                                .label(t("Browse", "浏览"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_browse_shell(cx);
                                })),
                        )
                        .into_any_element(),
                ),
                dropdown_field(
                    "shell-kind",
                    t("Kind", "类型"),
                    Some(t("Used to build the launch line", "用于拼接启动命令")),
                    &shell_kind,
                    &shell_options,
                    shell_kind_open,
                    |workspace, open, cx| workspace.on_toggle_shell_kind_menu(open, cx),
                    |workspace, kind, cx| {
                        workspace.on_set_shell_kind(kind, cx);
                        workspace.on_toggle_shell_kind_menu(false, cx);
                    },
                    cx,
                ),
            ],
        ))
        .into_any_element()
}
