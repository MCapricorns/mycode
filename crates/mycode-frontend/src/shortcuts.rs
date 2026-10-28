//! The keyboard shortcuts the desktop lists on its settings page.
//!
//! One table keeps that page in step with the keys the desktop binds.

use crate::i18n::t;

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

/// Every shortcut the desktop binds, in reading order.
#[must_use]
pub fn shortcuts() -> Vec<Shortcut> {
    // The desktop binds its chords to Command on macOS and Control elsewhere.
    let chord = if cfg!(target_os = "macos") {
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
                "While a task runs, it steers that task instead",
                "任务运行中则改为引导当前任务",
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
    list.extend([
        compose("Shift+Enter".to_owned(), t("New line", "换行"), None),
        // The clipboard reader answers "picture" or "no picture"; a paste
        // whose text is one image path attaches that file instead.
        compose(
            format!("{chord}V"),
            t("Attach the clipboard picture", "附加剪贴板里的图片"),
            Some(t(
                "A paste that is only an image path attaches that file",
                "粘贴内容只有图片路径时附加该文件",
            )),
        ),
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
    ]);
    list
}

#[cfg(test)]
mod tests {
    use super::{Group, shortcuts};

    #[test]
    fn send_interrupt_and_steer_come_first() {
        let keys: Vec<String> = shortcuts()
            .into_iter()
            .map(|shortcut| shortcut.keys)
            .collect();
        assert_eq!(keys[0], "Enter");
        assert!(keys[1].ends_with("Enter") && keys[1] != "Enter");
        assert!(keys[2].ends_with('G'));
    }

    #[test]
    fn the_clipboard_picture_key_is_listed() {
        let list = shortcuts();
        let paste = list
            .iter()
            .find(|shortcut| {
                matches!(
                    shortcut.action,
                    "Attach the clipboard picture" | "附加剪贴板里的图片"
                )
            })
            .expect("the picture paste is a documented key");
        assert!(paste.keys.ends_with('V'));
    }

    #[test]
    fn every_group_has_keys_and_compose_comes_first() {
        let list = shortcuts();
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

    #[test]
    fn a_sentence_joins_the_detail_to_the_action() {
        let list = shortcuts();
        let steer = &list[2];
        assert_eq!(
            steer.sentence(),
            format!("{} — {}", steer.action, steer.detail.expect("detail"))
        );
        let new_line = &list[3];
        assert_eq!(new_line.sentence(), new_line.action);
    }
}
