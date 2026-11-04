//! The working tree: the inspector preview, the full changes drawer, and
//! the numbered diff panel.
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use super::{
    CHANGES_PREVIEW, git_status_color, responsive_panel_bounds, review_panel, section_heading,
};
use crate::i18n::t;
use crate::workspace::Workspace;

pub(super) fn render_changes(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let git = workspace.git();
    let selected = workspace.git_diff_path();
    let shown: Vec<_> = git.files.iter().take(CHANGES_PREVIEW).collect();
    div()
        .id("changes")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(section_heading(t("Changes", "改动"), theme))
                .when(!git.files.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{}{} {}",
                                git.files.len(),
                                if git.stopped_early { "+" } else { "" },
                                t("files", "个文件")
                            )),
                    )
                }),
        )
        .when(!git.branch.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(git.branch.clone()),
            )
        })
        .when_some(git.note.clone(), |this, note| {
            this.child(div().text_xs().text_color(theme.muted_foreground).child(
                if workspace.vm().project_dir.is_none() {
                    t(
                        "Open a project to review changes.",
                        "打开项目后可查看文件改动。",
                    )
                    .to_owned()
                } else {
                    note
                },
            ))
        })
        .when(git.note.is_none() && git.files.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t("Working tree clean", "工作区无改动")),
            )
        })
        .children(shown.into_iter().enumerate().map(|(index, file)| {
            let path = file.path.clone();
            let open = selected == Some(file.path.as_str());
            let badge_color = git_status_color(&file.status, theme);
            div()
                .id(format!("git-file-{index}"))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(28.))
                .rounded(crate::ui::skin::radius_xs())
                .cursor_pointer()
                .when(open, |row| row.bg(theme.accent))
                .hover(|row| row.bg(theme.secondary_hover))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    cx.stop_propagation();
                    workspace.on_select_git_file(&path, cx);
                }))
                .child(
                    div()
                        .w(px(18.))
                        .text_xs()
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                        .text_color(badge_color)
                        .child(file.status.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(git_file_label(file)),
                )
        }))
        .when(!git.files.is_empty(), |this| {
            this.child(
                div()
                    .id("git-file-more")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(28.))
                    .rounded(px(6.))
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|row| row.bg(theme.secondary_hover).text_color(theme.foreground))
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_toggle_changes_panel(true, cx);
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(format!(
                        "{} {}{}",
                        t("View all", "查看全部"),
                        git.files.len(),
                        if git.stopped_early { "+" } else { "" }
                    )))
                    .child(Icon::new(IconName::ChevronRight).xsmall()),
            )
        })
}

/// The full changes drawer lists the dirty files the snapshot holds. The
/// selected file's diff opens in its own panel. The inspector panel is a
/// short preview; this is where a large working tree stays browsable.
pub(crate) fn render_changes_drawer(
    workspace: &mut Workspace,
    window: &Window,
    inspector_span: f32,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let git = workspace.git();
    let selected = workspace.git_diff_path();
    let diff_loaded = !workspace.git_diff().is_empty();
    let files = git.files.clone();
    let stopped_early = git.stopped_early;
    let viewport_width = f32::from(window.viewport_size().width);
    let file_count = format!(
        "{}{} {}",
        files.len(),
        if stopped_early { "+" } else { "" },
        t("files", "个文件")
    );
    let ((drawer_right, drawer_width), _) =
        responsive_panel_bounds(viewport_width, inspector_span, true);
    div()
        .id("changes-drawer-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("changes-drawer-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .bottom_0()
                // Leave the Details card uncovered so a click there is not
                // an outside-click on this drawer, and this scrim is not
                // what dismisses Details.
                .right(px(inspector_span))
                .bg(crate::ui::skin::menu_scrim(theme))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    cx.stop_propagation();
                    workspace.on_toggle_changes_panel(false, cx);
                    workspace.on_close_git_diff_panel(cx);
                })),
        )
        .child(
            // Same frame as the diff panel: the pair reads as one split view.
            review_panel("changes-drawer", drawer_right, drawer_width, theme)
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(div().flex_1().min_w_0().text_sm().truncate().child(format!(
                            "{}{}",
                            t("Changes", "改动"),
                            if git.branch.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", git.branch)
                            }
                        )))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(file_count),
                        )
                        .child(
                            div()
                                .id("changes-drawer-close")
                                .px_2()
                                .py(px(2.))
                                .rounded(px(6.))
                                .text_xs()
                                .cursor_pointer()
                                .text_color(theme.muted_foreground)
                                .hover(|this| this.bg(theme.secondary_hover))
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    cx.stop_propagation();
                                    workspace.on_toggle_changes_panel(false, cx);
                                }))
                                .child(t("Close", "关闭")),
                        ),
                )
                .child(
                    div()
                        .id("changes-drawer-files")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .children(files.into_iter().enumerate().map(|(index, file)| {
                            let path = file.path.clone();
                            let open = selected == Some(file.path.as_str());
                            let badge_color = git_status_color(&file.status, theme);
                            div()
                                .id(format!("changes-drawer-file-{index}"))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .flex_shrink_0()
                                .h(px(28.))
                                .rounded(crate::ui::skin::radius_xs())
                                .text_xs()
                                .cursor_pointer()
                                .when(open, |row| row.bg(theme.accent))
                                .hover(|row| row.bg(theme.secondary_hover))
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    cx.stop_propagation();
                                    workspace.on_select_git_file(&path, cx);
                                }))
                                .child(
                                    div()
                                        .w(px(20.))
                                        .flex_shrink_0()
                                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                                        .text_color(badge_color)
                                        .child(file.status.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(git_file_label(&file)),
                                )
                        }))
                        .when(stopped_early, |list| {
                            list.child(
                                div()
                                    .px_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(t(
                                        "More changes are not listed.",
                                        "还有更多改动未列出。",
                                    )),
                            )
                        }),
                )
                .child(
                    div()
                        .id("changes-drawer-diff")
                        .flex_shrink_0()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if selected.is_none() {
                            t("Pick a file to open its diff.", "选择一个文件以打开差异。")
                                .to_owned()
                        } else if !diff_loaded {
                            t("Opening the diff panel…", "正在打开差异面板…").to_owned()
                        } else {
                            t(
                                "Diff is open in its own panel.",
                                "差异已在单独的面板中打开。",
                            )
                            .to_owned()
                        }),
                ),
        )
        .into_any_element()
}

/// Dedicated diff panel. File clicks in the changes list and in the review-all
/// drawer both open this, so the patch is not clipped inside the inspector.
pub(crate) fn render_diff_panel(
    workspace: &mut Workspace,
    window: &Window,
    inspector_span: f32,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let path = workspace
        .git_diff_path()
        .unwrap_or(t("Diff", "差异"))
        .to_owned();
    let rows = numbered_diff_rows(workspace.git_diff());
    let (added, removed) = diff_line_counts(&rows);
    let viewport_width = f32::from(window.viewport_size().width);
    let (_, (diff_right, diff_width)) = responsive_panel_bounds(
        viewport_width,
        inspector_span,
        workspace.vm().changes_panel_open,
    );
    // The drawer's own scrim already owns outside clicks while it is open; a
    // second scrim here would cover the drawer and steal file clicks. With the
    // drawer closed this layer is what makes a click anywhere left of the diff
    // close it.
    let mut layer = div().id("diff-panel-scrim-layer").absolute().inset_0();
    if !workspace.vm().changes_panel_open {
        layer = layer.child(
            div()
                .id("diff-panel-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .bottom_0()
                .right(px(diff_right))
                .bg(crate::ui::skin::menu_scrim(theme))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    cx.stop_propagation();
                    workspace.on_close_git_diff_panel(cx);
                })),
        );
    }
    layer
        .child(
            review_panel("diff-panel-layer", diff_right, diff_width, theme)
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(div().flex_1().min_w_0().text_sm().truncate().child(path))
                        .when(added + removed > 0, |this| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .flex()
                                    .flex_row()
                                    .gap(px(6.))
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(
                                        div()
                                            .text_color(crate::ui::desk::diff_ink(theme, true))
                                            .child(format!("+{added}")),
                                    )
                                    .child(
                                        div()
                                            .text_color(crate::ui::desk::diff_ink(theme, false))
                                            .child(format!("−{removed}")),
                                    ),
                            )
                        })
                        .child(
                            div()
                                .id("diff-panel-close")
                                .px_2()
                                .py(px(2.))
                                .rounded(px(6.))
                                .text_xs()
                                .cursor_pointer()
                                .text_color(theme.muted_foreground)
                                .hover(|this| this.bg(theme.secondary_hover))
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    cx.stop_propagation();
                                    workspace.on_close_git_diff_panel(cx);
                                }))
                                .child(t("Close", "关闭")),
                        ),
                )
                .child(
                    div()
                        .id("diff-panel-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .py_2()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .children(rows.into_iter().enumerate().map(|(index, row)| {
                            let (color, bg) = diff_line_paint(row.kind, theme);
                            div()
                                .id(format!("diff-line-{index}"))
                                .flex()
                                .flex_row()
                                .px_2()
                                .text_color(color)
                                .bg(bg)
                                .child(
                                    div()
                                        .w(px(36.))
                                        .flex_shrink_0()
                                        .text_right()
                                        .text_color(theme.muted_foreground)
                                        .child(row.number),
                                )
                                .child(div().min_w_0().pl_2().child(if row.text.is_empty() {
                                    " ".to_owned()
                                } else {
                                    row.text.to_owned()
                                }))
                        })),
                ),
        )
        .into_any_element()
}

struct NumberedDiffRow<'a> {
    number: String,
    text: &'a str,
    kind: DiffLineKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffLineKind {
    Added,
    Removed,
    Hunk,
    Other,
}

/// One row per diff line with its gutter number; a diff still loading is a
/// single placeholder row.
fn numbered_diff_rows(diff: &str) -> Vec<NumberedDiffRow<'_>> {
    if diff.is_empty() {
        return vec![NumberedDiffRow {
            number: String::new(),
            text: t("Loading diff…", "正在加载差异…"),
            kind: DiffLineKind::Other,
        }];
    }
    let mut old_n = 1u64;
    let mut new_n = 1u64;
    let mut in_hunk = false;
    let mut rows = Vec::new();
    for line in diff.lines() {
        if let Some((old, new)) = crate::ui::chat::parse_diff_hunk_header(line) {
            old_n = u64::from(old);
            new_n = u64::from(new);
            in_hunk = true;
            rows.push(NumberedDiffRow {
                number: String::new(),
                text: line,
                kind: DiffLineKind::Hunk,
            });
            continue;
        }
        if line.starts_with("diff ") {
            in_hunk = false;
        }
        // File headers have no line number. Within a hunk, even `+++` and
        // `---` are source lines; only the first sign is the diff marker.
        let added = in_hunk && line.starts_with('+');
        let removed = in_hunk && line.starts_with('-');
        let number = if added {
            let value = new_n.to_string();
            new_n += 1;
            value
        } else if removed {
            let value = old_n.to_string();
            old_n += 1;
            value
        } else if in_hunk && line.starts_with(' ') {
            let value = new_n.to_string();
            old_n += 1;
            new_n += 1;
            value
        } else {
            // Metadata, including `\ No newline at end of file`, does not
            // consume either file's line counter.
            String::new()
        };
        let kind = if added {
            DiffLineKind::Added
        } else if removed {
            DiffLineKind::Removed
        } else {
            DiffLineKind::Other
        };
        rows.push(NumberedDiffRow {
            number,
            text: line,
            kind,
        });
    }
    rows
}

/// Added and removed line counts, file headers excluded.
fn diff_line_counts(rows: &[NumberedDiffRow<'_>]) -> (usize, usize) {
    rows.iter()
        .fold((0, 0), |(added, removed), row| match row.kind {
            DiffLineKind::Added => (added + 1, removed),
            DiffLineKind::Removed => (added, removed + 1),
            _ => (added, removed),
        })
}

fn diff_line_paint(kind: DiffLineKind, theme: &Theme) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    if kind == DiffLineKind::Added {
        let ink = crate::ui::desk::diff_ink(theme, true);
        return (ink, crate::ui::desk::diff_wash(ink));
    }
    if kind == DiffLineKind::Removed {
        let ink = crate::ui::desk::diff_ink(theme, false);
        return (ink, crate::ui::desk::diff_wash(ink));
    }
    if kind == DiffLineKind::Hunk {
        return (theme.primary, theme.transparent);
    }
    (theme.muted_foreground, theme.transparent)
}

fn git_file_label(file: &crate::git_status::GitFile) -> String {
    match &file.previous {
        Some(previous) => format!("{previous} -> {}", file.path),
        None => file.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{DiffLineKind, diff_line_counts, numbered_diff_rows};

    #[test]
    fn diff_metadata_does_not_consume_source_lines_or_hide_sign_prefixed_source() {
        let diff = "diff --git a/source b/source\n\
                    new file mode 100644\n\
                    --- a/source\n\
                    +++ b/source\n\
                    @@ -7,2 +9,2 @@\n\
                    ---old\n\
                    \\ No newline at end of file\n\
                    +++new\n\
                    \\ No newline at end of file\n \
                    context\n\
                    diff --git a/next b/next\n\
                    --- a/next\n\
                    +++ b/next\n\
                    @@ -1 +1 @@\n\
                    -before\n\
                    +after\n";
        let rows = numbered_diff_rows(diff);
        let numbered: Vec<_> = rows
            .iter()
            .filter(|row| !row.number.is_empty())
            .map(|row| (row.text, row.number.as_str()))
            .collect();
        assert_eq!(
            numbered,
            [
                ("---old", "7"),
                ("+++new", "9"),
                (" context", "10"),
                ("-before", "1"),
                ("+after", "1")
            ]
        );
        assert_eq!(diff_line_counts(&rows), (2, 2));
        assert!(rows.iter().find(|row| row.text == "+++new").unwrap().kind == DiffLineKind::Added);
        assert!(
            rows.iter().find(|row| row.text == "---old").unwrap().kind == DiffLineKind::Removed
        );
    }
}
