//! Global command palette. Cmd-K on macOS and Ctrl-K on Windows.
//!
//! The list is the view-model catalog. This module only paints it and
//! recognizes the shortcut. Confirming a row goes back through the workspace
//! handlers that already open sessions, pick models, run slash commands, and
//! change settings pages.

use gpui_kit::assets::IconName;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, Keystroke, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::i18n::t;
use crate::view_model::{
    PaletteCatalog, PaletteCommand, PaletteGroup, PaletteGroupId, PaletteItem, palette_catalog,
};
use crate::workspace::Workspace;

use super::skin::menu_panel;

gpui_kit::actions!(mycode, [ToggleCommandPalette]);

/// Cmd on macOS, Ctrl on Windows, with no other modifiers.
#[must_use]
pub(crate) fn is_command_palette_shortcut(keystroke: &Keystroke) -> bool {
    if !keystroke.key.eq_ignore_ascii_case("k") {
        return false;
    }
    let modifiers = keystroke.modifiers;
    if !modifiers.secondary() || modifiers.shift || modifiers.alt || modifiers.function {
        return false;
    }
    // `secondary` is Command on macOS and Control elsewhere. Reject the other
    // modifier so Command+Control+K does not open the palette.
    #[cfg(target_os = "macos")]
    {
        !modifiers.control
    }
    #[cfg(not(target_os = "macos"))]
    {
        !modifiers.platform
    }
}

/// Shortcut spelling for the palette chrome and the docs.
#[must_use]
pub(crate) fn shortcut_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘K"
    } else {
        "Ctrl+K"
    }
}

pub(super) fn render(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(state) = workspace.command_palette_entity() else {
        return div().into_any_element();
    };
    if workspace.take_palette_focus() {
        state.update(cx, |palette, cx| palette.focus(window, cx));
    }
    let theme = cx.theme().clone();
    let catalog = palette_catalog(workspace.vm());
    let weak = cx.weak_entity();
    let command = command_element(&state, &catalog, weak);
    let scrim = crate::ui::skin::modal_scrim(&theme);
    div()
        .id("command-palette-layer")
        .absolute()
        .inset_0()
        .occlude()
        .child(
            div()
                .id("command-palette-scrim")
                .absolute()
                .size_full()
                .bg(scrim)
                .on_click(cx.listener(|workspace, _, window, cx| {
                    workspace.set_command_palette(false, window, cx);
                })),
        )
        .child(
            div()
                .absolute()
                .top(px(72.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .px_4()
                .child(
                    menu_panel("command-palette", &theme)
                        .w(px(560.))
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(command),
                ),
        )
        .into_any_element()
}

fn command_element(
    state: &gpui_kit::Entity<CommandState>,
    catalog: &PaletteCatalog,
    weak: gpui_kit::WeakEntity<Workspace>,
) -> Command {
    let mut command = Command::new(state)
        .placeholder(t(
            "Search sessions, models, commands, settings",
            "搜索会话、模型、命令、设置",
        ))
        .bordered(false)
        .max_h(px(380.))
        .w_full()
        .empty(|_, _, _| {
            div()
                .py_6()
                .w_full()
                .text_center()
                .text_sm()
                .child(t("No matches", "没有匹配"))
        })
        .header(|_, _, cx| {
            let theme = cx.theme();
            div()
                .px_3()
                .pt_2()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t("Command palette", "命令面板"))
                .child(shortcut_label())
        })
        .footer(|_, _, cx| {
            let theme = cx.theme();
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(footer_hint())
        })
        .on_confirm(move |index, window, cx| {
            let section = index.section;
            let row = index.row;
            let _ = weak.update(cx, |workspace, cx| {
                workspace.activate_palette(section, row, window, cx);
            });
        });
    let mut first = true;
    for group in &catalog.groups {
        if !first {
            command = command.separator();
        }
        first = false;
        command = command.group(command_group(group));
    }
    command
}

fn command_group(group: &PaletteGroup) -> CommandGroup {
    let mut built = CommandGroup::new().label(group_heading(group.id));
    for item in &group.items {
        built = built.item(command_item(item));
    }
    built
}

fn command_item(item: &PaletteItem) -> CommandItem {
    CommandItem::new()
        .label(item.label.clone())
        .keywords(item.keywords.iter().cloned())
        .icon(Icon::new(item_icon(&item.command)))
        .checked(item.checked)
}

fn group_heading(id: PaletteGroupId) -> &'static str {
    match id {
        PaletteGroupId::Sessions => t("Sessions", "会话"),
        PaletteGroupId::Models => t("Models", "模型"),
        PaletteGroupId::Commands => t("Commands", "命令"),
        PaletteGroupId::Settings => t("Settings", "设置"),
    }
}

fn item_icon(command: &PaletteCommand) -> IconName {
    match command {
        PaletteCommand::NewSession => IconName::Plus,
        PaletteCommand::OpenSession(_) => IconName::MessageSquare,
        PaletteCommand::SelectModel { .. } => IconName::Bot,
        PaletteCommand::Slash(insert) => {
            if insert.starts_with("mcp:") {
                IconName::PlugZap
            } else if matches!(insert.as_str(), "/new" | "/compact" | "/settings") {
                IconName::Terminal
            } else {
                IconName::Sparkles
            }
        }
        PaletteCommand::OpenSettings(section) => crate::ui::section_icon(*section),
    }
}

fn footer_hint() -> String {
    let keys = shortcut_label();
    if crate::i18n::is_chinese() {
        format!("↑↓ 移动 · Enter 打开 · Esc 关闭 · {keys}")
    } else {
        format!("↑↓ move · Enter open · Esc close · {keys}")
    }
}

#[cfg(test)]
mod tests {
    use super::is_command_palette_shortcut;
    use gpui_kit::Keystroke;

    #[test]
    fn secondary_k_toggles_the_palette_and_other_chords_do_not() {
        let open = Keystroke::parse("secondary-k").expect("shortcut");
        assert!(is_command_palette_shortcut(&open));
        assert!(!is_command_palette_shortcut(
            &Keystroke::parse("k").expect("plain k")
        ));
        assert!(!is_command_palette_shortcut(
            &Keystroke::parse("secondary-shift-k").expect("shifted")
        ));
        assert!(!is_command_palette_shortcut(
            &Keystroke::parse("alt-secondary-k").expect("alt")
        ));
        // The platform modifier that is not the secondary key (Super / Win,
        // or Control on macOS) is a different chord.
        let other = if cfg!(target_os = "macos") {
            Keystroke::parse("ctrl-k").expect("control")
        } else {
            Keystroke::parse("cmd-k").expect("super")
        };
        assert!(!is_command_palette_shortcut(&other));
    }
}
