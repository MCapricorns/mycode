//! In-app folder browser. Project selection stays inside GPUI and still binds
//! a real directory path through the existing session flow.
//!
//! The first screen is the drive list, so a session that starts on `C:` can
//! still switch to another drive. Footer actions use the same glass button
//! as the welcome screen.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};

use super::skin;
use crate::workspace::Workspace;

const MAX_ENTRIES: usize = 400;

/// One browsable directory or file row.
#[derive(Clone, Debug)]
pub(crate) struct PickerEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Ephemeral folder-browser state. Listing happens when the user navigates,
/// not inside the reducer.
#[derive(Clone, Debug)]
pub(crate) struct ProjectPicker {
    pub current: Option<PathBuf>,
    pub entries: Vec<PickerEntry>,
    pub status: Option<String>,
}

impl ProjectPicker {
    /// Opens at the drive / filesystem-root list.
    pub(crate) fn open() -> Self {
        browse(None)
    }

    /// Opens the user profile, matching the Home control.
    pub(crate) fn home() -> Self {
        browse(Some(starting_directory()))
    }
}

pub(crate) fn render(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme().clone();
    let picker = workspace
        .project_picker
        .clone()
        .unwrap_or_else(ProjectPicker::open);
    let at_roots = picker.current.is_none();
    let path_label = picker
        .current
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| crate::i18n::t("This computer", "此电脑").to_owned());
    let drives = filesystem_roots();
    let current = picker.current.clone();
    let entries = picker.entries;
    let status = picker.status;
    div()
        .id("project-picker-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("project-picker-scrim")
                .absolute()
                .size_full()
                .bg(skin::scrim(&theme))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_picker_cancel(cx);
                })),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("project-picker")
                        .occlude()
                        .w(px(520.))
                        .h(px(480.))
                        .flex()
                        .flex_col()
                        .rounded(skin::radius_card())
                        .border_1()
                        .border_color(skin::glass_border(&theme))
                        .bg(skin::popover(&theme))
                        .text_color(theme.foreground)
                        .overflow_hidden()
                        .on_click(|_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(picker_header(&theme, &path_label))
                        .child(picker_nav(cx, at_roots))
                        .child(drive_strip(cx, &drives, current.as_deref(), &theme))
                        .child(picker_list(cx, &entries, status.as_deref(), &theme))
                        .child(picker_footer(cx, at_roots, &theme)),
                ),
        )
        .into_any_element()
}

fn picker_header(theme: &gpui_kit::component::theme::Theme, path_label: &str) -> impl IntoElement {
    div()
        .px_4()
        .pt_3()
        .pb_2()
        .flex()
        .flex_col()
        .gap_1()
        .border_b_1()
        .border_color(skin::glass_border(theme))
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(crate::i18n::t("Choose a project folder", "选择项目目录")),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .child(path_label.to_owned()),
        )
}

fn picker_nav(cx: &mut Context<Workspace>, at_roots: bool) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .px_3()
        .py_2()
        .flex()
        .flex_row()
        .gap_2()
        .child(
            desk_button(
                "picker-up",
                crate::i18n::t("Up", "上一级"),
                !at_roots,
                theme,
            )
            .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.on_picker_up(cx);
            })),
        )
        .child(
            desk_button("picker-home", crate::i18n::t("Home", "主目录"), true, theme).on_click(
                cx.listener(|workspace, _, _, cx| {
                    workspace.on_picker_home(cx);
                }),
            ),
        )
        .child(
            desk_button(
                "picker-roots",
                crate::i18n::t("This computer", "此电脑"),
                !at_roots,
                theme,
            )
            .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.on_picker_roots(cx);
            })),
        )
}

fn drive_strip(
    cx: &mut Context<Workspace>,
    drives: &[PathBuf],
    current: Option<&Path>,
    theme: &gpui_kit::component::theme::Theme,
) -> impl IntoElement {
    div()
        .id("project-picker-drives")
        .px_3()
        .pb_1()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_1()
        .children(drives.iter().enumerate().map(|(index, drive)| {
            let path = drive.clone();
            let selected = drive_selected(current, drive);
            let label = drive_label(drive);
            div()
                .id(format!("picker-drive-{index}"))
                .px_3()
                .h(px(28.))
                .flex()
                .items_center()
                .rounded(px(8.))
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .border_1()
                .border_color(if selected {
                    theme.primary
                } else {
                    theme.border
                })
                .bg(if selected {
                    theme.accent
                } else {
                    theme.secondary
                })
                .text_color(theme.foreground)
                .cursor_pointer()
                .hover(|style| style.bg(theme.secondary_hover))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_picker_enter(path.clone(), cx);
                }))
                .child(label)
        }))
}

fn picker_list(
    cx: &mut Context<Workspace>,
    entries: &[PickerEntry],
    status: Option<&str>,
    theme: &gpui_kit::component::theme::Theme,
) -> impl IntoElement {
    div()
        .id("project-picker-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_2()
        .py_1()
        .flex()
        .flex_col()
        .gap_0p5()
        .children(entries.iter().enumerate().map(|(index, entry)| {
            let path = entry.path.clone();
            let name = entry.name.clone();
            let is_dir = entry.is_dir;
            div()
                .id(format!("picker-row-{index}"))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(6.))
                .rounded(px(6.))
                .when(is_dir, |row| {
                    row.cursor_pointer()
                        .hover(|style| style.bg(skin::frost_hover(theme)))
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.on_picker_enter(path.clone(), cx);
                        }))
                })
                .child(
                    Icon::new(if is_dir {
                        IconName::FolderOpen
                    } else {
                        IconName::File
                    })
                    .with_size(px(15.))
                    .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .text_color(if is_dir {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(name),
                )
        }))
        .when(entries.is_empty(), |list| {
            list.child(
                div()
                    .px_2()
                    .py_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(status.map(str::to_owned).unwrap_or_else(|| {
                        crate::i18n::t("This folder is empty.", "此目录为空。").to_owned()
                    })),
            )
        })
        .when(status.is_some() && !entries.is_empty(), |list| {
            list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status.unwrap_or_default().to_owned()),
            )
        })
}

fn picker_footer(
    cx: &mut Context<Workspace>,
    at_roots: bool,
    theme: &gpui_kit::component::theme::Theme,
) -> impl IntoElement {
    div()
        .px_3()
        .py_3()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .border_t_1()
        .border_color(skin::glass_border(theme))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(crate::i18n::t(
                    "Drop a folder on the window to open it.",
                    "把目录拖到窗口即可打开。",
                )),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    desk_button(
                        "picker-cancel",
                        crate::i18n::t("Cancel", "取消"),
                        true,
                        theme,
                    )
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_picker_cancel(cx);
                    })),
                )
                .child(
                    desk_button(
                        "picker-use",
                        crate::i18n::t("Use this folder", "使用此目录"),
                        !at_roots,
                        theme,
                    )
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        if !at_roots {
                            workspace.on_picker_confirm(cx);
                        }
                    })),
                ),
        )
}

/// Same glass, ink, and border as the welcome actions.
fn desk_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    theme: &gpui_kit::component::theme::Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    skin::glass_button(id, false, theme)
        .when(!enabled, |this| this.opacity(0.45))
        .text_color(if enabled {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .child(label)
}

/// Parent directory, or `None` at a filesystem root.
pub(crate) fn parent_folder(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() || parent == path {
        return None;
    }
    Some(parent.to_path_buf())
}

pub(crate) fn browse(path: Option<PathBuf>) -> ProjectPicker {
    let Some(path) = path else {
        return ProjectPicker {
            current: None,
            entries: filesystem_roots()
                .into_iter()
                .map(|root| PickerEntry {
                    name: drive_label(&root),
                    path: root,
                    is_dir: true,
                })
                .collect(),
            status: None,
        };
    };
    match list_entries(&path) {
        Ok((entries, truncated)) => ProjectPicker {
            current: Some(path),
            entries,
            status: truncated.then(|| {
                format!(
                    "{}{MAX_ENTRIES} {}",
                    crate::i18n::t("Showing the first ", "显示前 "),
                    crate::i18n::t("entries.", "条。")
                )
            }),
        },
        Err(status) => ProjectPicker {
            current: Some(path),
            entries: Vec::new(),
            status: Some(status),
        },
    }
}

fn starting_directory() -> PathBuf {
    for key in ["USERPROFILE", "HOME"] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            if path.is_dir() {
                return path;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn list_entries(path: &Path) -> Result<(Vec<PickerEntry>, bool), String> {
    let mut read = std::fs::read_dir(path).map_err(|error| error.to_string())?;
    let mut folders = Vec::new();
    let mut files = Vec::new();
    let mut truncated = false;
    for item in read.by_ref() {
        let Ok(item) = item else {
            continue;
        };
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        let name = item.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        let entry = PickerEntry {
            name,
            path: item.path(),
            is_dir: file_type.is_dir(),
        };
        if entry.is_dir {
            folders.push(entry);
        } else {
            files.push(entry);
        }
        if folders.len() + files.len() >= MAX_ENTRIES {
            // Ask the iterator, not the count: a directory with exactly
            // MAX_ENTRIES entries is fully shown and must not claim it was
            // truncated.
            truncated = read.filter_map(std::result::Result::ok).next().is_some();
            break;
        }
    }
    let by_name = |left: &PickerEntry, right: &PickerEntry| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    };
    folders.sort_by(by_name);
    files.sort_by(by_name);
    folders.append(&mut files);
    Ok((folders, truncated))
}

fn drive_label(path: &Path) -> String {
    let text = path.display().to_string();
    #[cfg(windows)]
    {
        let bytes = text.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            return text[..2].to_owned();
        }
    }
    text
}

fn drive_selected(current: Option<&Path>, drive: &Path) -> bool {
    let Some(current) = current else {
        return false;
    };
    let current = current
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let drive = drive
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase();
    !drive.is_empty() && current.starts_with(&drive)
}

fn filesystem_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        // SAFETY: GetLogicalDrives takes no pointers and has no preconditions.
        let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        (0..26)
            .filter_map(|index| {
                if mask & (1 << index) == 0 {
                    return None;
                }
                let letter = (b'A' + index as u8) as char;
                Some(PathBuf::from(format!("{letter}:\\")))
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}
