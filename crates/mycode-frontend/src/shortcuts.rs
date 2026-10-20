//! The keyboard shortcuts each frontend lists on its settings page.
//!
//! One table per surface keeps both settings pages in step with the keys
//! the frontends bind. The terminal's keys differ where a terminal cannot
//! report a key a window sees.

use crate::i18n::t;

/// The frontend a list is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Desktop,
    Terminal,
}

/// One shortcut as a settings page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// Keys as pressed, spelled for this platform (`⌘G` on macOS).
    pub keys: String,
    /// What they do, in the interface language.
    pub action: &'static str,
}

/// Every shortcut `surface` binds, in reading order.
#[must_use]
pub fn shortcuts(surface: Surface) -> Vec<Shortcut> {
    // The desktop binds its chords to Command on macOS; a terminal only
    // ever sees Control.
    let chord = if surface == Surface::Desktop && cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    };
    let entry = |keys: String, action| Shortcut { keys, action };
    let mut list = vec![
        entry(
            "Enter".to_owned(),
            t(
                "Send; while a task runs, queue for after it",
                "发送；任务运行中则排队，等它结束再发",
            ),
        ),
        entry(
            format!("{chord}Enter"),
            t(
                "Stop the running task and send now",
                "停止当前任务并立即发送",
            ),
        ),
        entry(
            format!("{chord}G"),
            t(
                "Steer the running task: it reads the message before its next step, without stopping",
                "引导当前任务：不打断，模型在下一步之前读到这条消息",
            ),
        ),
    ];
    match surface {
        Surface::Desktop => list.extend([
            entry("Shift+Enter".to_owned(), t("New line", "换行")),
            entry(
                "Esc".to_owned(),
                t(
                    "Close the topmost panel, or stop the running task",
                    "关闭最上层面板，或停止当前任务",
                ),
            ),
            entry(format!("{chord}K"), t("Command palette", "命令面板")),
        ]),
        Surface::Terminal => list.extend([
            entry("Shift+Enter / Ctrl+J".to_owned(), t("New line", "换行")),
            entry(
                "Esc".to_owned(),
                t(
                    "Close the topmost menu or page, or stop the running task",
                    "关闭最上层菜单或页面，或停止当前任务",
                ),
            ),
            entry(
                "Ctrl+C".to_owned(),
                t(
                    "Stop the running task; quit when idle",
                    "停止当前任务；空闲时退出",
                ),
            ),
            entry("Ctrl+N".to_owned(), t("New task", "新任务")),
            entry(
                "Ctrl+O".to_owned(),
                t(
                    "Fold or expand every thinking trace",
                    "折叠或展开所有思考记录",
                ),
            ),
            entry(
                "PageUp / PageDown".to_owned(),
                t("Scroll the transcript", "滚动对话"),
            ),
        ]),
    }
    list
}

#[cfg(test)]
mod tests {
    use super::{Surface, shortcuts};

    #[test]
    fn both_surfaces_list_send_interrupt_and_steer() {
        for surface in [Surface::Desktop, Surface::Terminal] {
            let keys: Vec<String> = shortcuts(surface)
                .into_iter()
                .map(|shortcut| shortcut.keys)
                .collect();
            assert_eq!(keys[0], "Enter");
            assert!(keys[1].ends_with("Enter") && keys[1] != "Enter");
            assert!(keys[2].ends_with('G'));
        }
    }

    #[test]
    fn the_terminal_spells_control_on_every_platform() {
        let list = shortcuts(Surface::Terminal);
        assert_eq!(list[1].keys, "Ctrl+Enter");
        assert_eq!(list[2].keys, "Ctrl+G");
    }
}
