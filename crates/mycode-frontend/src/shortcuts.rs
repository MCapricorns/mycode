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

/// Where a shortcut acts. The desktop lists each group on its own card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Keys the message box takes.
    Compose,
    /// Keys that move around the interface.
    Navigate,
}

impl Group {
    /// Every group in reading order.
    pub const ALL: [Self; 2] = [Self::Compose, Self::Navigate];

    /// Stable identifier for element ids.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Compose => "compose",
            Self::Navigate => "navigate",
        }
    }

    /// Card title.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Compose => t("Compose and send", "撰写与发送"),
            Self::Navigate => t("Navigation", "界面与导航"),
        }
    }

    /// Where the keys in this group work.
    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            Self::Compose => t(
                "In the message box at the bottom of a chat.",
                "在对话底部的输入框里。",
            ),
            Self::Navigate => t("Anywhere in the window.", "在窗口任意位置。"),
        }
    }
}

/// One shortcut as a settings page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// Keys as pressed, spelled for this platform (`⌘G` on macOS).
    pub keys: String,
    /// What they do, in the interface language.
    pub action: &'static str,
    /// When or how, for keys whose effect depends on the moment.
    pub detail: Option<&'static str>,
    /// The card it is listed on.
    pub group: Group,
}

impl Shortcut {
    /// The action and its detail on one line, for a list with one column
    /// of text.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self.detail {
            Some(detail) => format!("{} — {detail}", self.action),
            None => self.action.to_owned(),
        }
    }
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
    let entry = |keys: String, action, detail, group| Shortcut {
        keys,
        action,
        detail,
        group,
    };
    let compose = |keys, action, detail| entry(keys, action, detail, Group::Compose);
    let navigate = |keys, action, detail| entry(keys, action, detail, Group::Navigate);
    let mut list = vec![
        compose(
            "Enter".to_owned(),
            t("Send", "发送"),
            Some(t(
                "While a task runs, the message waits in the queue until it ends",
                "任务运行中则排队，等它结束再发",
            )),
        ),
        compose(
            format!("{chord}Enter"),
            t("Stop and send now", "停止并立即发送"),
            Some(t(
                "Stops the running task, then sends the message",
                "先停止当前任务，再发送这条消息",
            )),
        ),
        compose(
            format!("{chord}G"),
            t("Steer the running task", "引导当前任务"),
            Some(t(
                "It reads the message before its next step, without stopping",
                "不打断，模型在下一步之前读到这条消息",
            )),
        ),
    ];
    match surface {
        Surface::Desktop => list.extend([
            compose("Shift+Enter".to_owned(), t("New line", "换行"), None),
            navigate(
                format!("{chord}K"),
                t("Command palette", "命令面板"),
                Some(t(
                    "Jump to a session, model, command, or settings page",
                    "跳到会话、模型、命令或设置页",
                )),
            ),
            navigate(
                "Esc".to_owned(),
                t("Close the topmost panel", "关闭最上层面板"),
                Some(t(
                    "With nothing open, leaves a running task alone; a held queue returns to the input",
                    "没有打开的面板时，不打断运行中的任务；暂停的队列退回输入框",
                )),
            ),
        ]),
        Surface::Terminal => {
            list.extend([
                compose(
                    "Shift+Enter / Ctrl+J".to_owned(),
                    t("New line", "换行"),
                    None,
                ),
            ]);
            // A picture paste needs a real clipboard reader; the stubs on
            // other platforms always answer "no picture".
            if cfg!(any(windows, target_os = "macos")) {
                list.extend([
                    compose(
                        "Ctrl+V".to_owned(),
                        t("Attach the clipboard picture", "附加剪贴板里的图片"),
                        Some(t(
                            "A paste that is only an image path attaches that file",
                            "粘贴内容只有图片路径时附加该文件",
                        )),
                    ),
                    compose(
                        "Ctrl+X".to_owned(),
                        t("Drop the waiting attachments", "清除待发送的附件"),
                        None,
                    ),
                ]);
            }
            list.extend([
                navigate(
                "Esc".to_owned(),
                t("Close the topmost menu or page", "关闭最上层菜单或页面"),
                Some(t(
                    "With nothing open, stops the running task",
                    "没有打开的菜单时，停止当前任务",
                )),
            ),
            navigate(
                "Ctrl+C".to_owned(),
                t("Stop the running task", "停止当前任务"),
                Some(t("Quits when idle", "空闲时退出")),
            ),
            navigate("Ctrl+N".to_owned(), t("New task", "新任务"), None),
            navigate(
                "Ctrl+O".to_owned(),
                t(
                    "Fold or expand every thinking trace",
                    "折叠或展开所有思考记录",
                ),
                None,
            ),
            navigate(
                "Wheel / PageUp / PageDown".to_owned(),
                t("Scroll the transcript", "滚动对话"),
                None,
            ),
            ]);
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::{Group, Surface, shortcuts};

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

    #[test]
    fn only_the_terminal_lists_the_clipboard_picture_keys() {
        let terminal: Vec<String> = shortcuts(Surface::Terminal)
            .into_iter()
            .map(|shortcut| shortcut.keys)
            .collect();
        let desktop: Vec<String> = shortcuts(Surface::Desktop)
            .into_iter()
            .map(|shortcut| shortcut.keys)
            .collect();
        if cfg!(any(windows, target_os = "macos")) {
            assert!(terminal.iter().any(|keys| keys == "Ctrl+V"));
            assert!(terminal.iter().any(|keys| keys == "Ctrl+X"));
        } else {
            assert!(!terminal.iter().any(|keys| keys == "Ctrl+V"));
        }
        assert!(!desktop.iter().any(|keys| keys == "Ctrl+V"));
    }

    #[test]
    fn every_group_has_keys_and_compose_comes_first() {
        for surface in [Surface::Desktop, Surface::Terminal] {
            let list = shortcuts(surface);
            for group in Group::ALL {
                assert!(list.iter().any(|shortcut| shortcut.group == group));
            }
            let first_navigate = list
                .iter()
                .position(|shortcut| shortcut.group == Group::Navigate)
                .expect("navigation keys");
            assert!(
                list[first_navigate..]
                    .iter()
                    .all(|shortcut| shortcut.group == Group::Navigate)
            );
        }
    }

    #[test]
    fn a_sentence_joins_the_detail_to_the_action() {
        let list = shortcuts(Surface::Terminal);
        let steer = &list[2];
        assert_eq!(
            steer.sentence(),
            format!("{} — {}", steer.action, steer.detail.expect("detail"))
        );
        let new_line = &list[3];
        assert_eq!(new_line.sentence(), new_line.action);
    }
}
