//! Empty-desk welcome. A gradient wordmark, actions, and recent folders.
//!
//! With no folder open there is one action: Open folder. Once a folder is
//! open, a second outline action starts a task. Both surfaces share
//! [`new_task_label`], so a rename is one edit.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, Context, HighlightStyle, Hsla, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, StyledText, div, ease_out_quint, px,
};

use crate::i18n::t;
use crate::ui::{element_id, hover_delete_button, project_label, skin};
use crate::view_model::has_open_folder;
use crate::workspace::Workspace;

/// Shared label for a new task: the empty-desk action, the sidebar
/// control, and an untitled session row.
const NEW_TASK_LABEL: (&str, &str) = ("New task", "新建任务");

const WORDMARK: &str = "MYCode";

/// One color sweep across the wordmark when the desk mounts.
const WORDMARK_SWEEP_MS: u64 = 1400;

/// Share of the cyclic ramp the resting wordmark spans: blue to violet,
/// leaving the pink-to-blue return for the sweep.
const WORDMARK_SPAN: f32 = 0.75;

const TAGLINE_NO_FOLDER: (&str, &str) = ("Open a folder to get started.", "打开一个目录即可开始。");
const TAGLINE_HAS_FOLDER: (&str, &str) = (
    "Start a task in this workspace.",
    "在此工作区开始一个任务。",
);

/// The new-task label. The empty desk, the sidebar control, and untitled
/// session rows all call this.
#[must_use]
pub(crate) fn new_task_label() -> &'static str {
    t(NEW_TASK_LABEL.0, NEW_TASK_LABEL.1)
}

fn empty_desk_tagline(has_folder: bool) -> (&'static str, &'static str) {
    if has_folder {
        TAGLINE_HAS_FOLDER
    } else {
        TAGLINE_NO_FOLDER
    }
}

pub(super) fn render_welcome(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    let has_folder = has_open_folder(workspace.vm());
    let (tagline_en, tagline_zh) = empty_desk_tagline(has_folder);
    div()
        .id("welcome")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .py(px(72.))
        .child(
            div()
                .id("welcome-title")
                .text_3xl()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child(render_wordmark(theme.mode.is_dark(), cx.reduce_motion())),
        )
        .child(
            div()
                .id("welcome-tagline")
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t(tagline_en, tagline_zh)),
        )
        .child(
            div()
                .id("welcome-actions")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .mt_2()
                .child(
                    welcome_action(
                        "welcome-open-project",
                        IconName::FolderOpen,
                        t("Open folder", "打开目录"),
                        theme,
                    )
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_open_project_dialog(cx);
                    })),
                )
                .when(has_folder, |actions| {
                    actions.child(
                        welcome_action(
                            "welcome-new-task",
                            IconName::MessageSquare,
                            new_task_label(),
                            theme,
                        )
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_new_session(cx);
                        })),
                    )
                }),
        )
        .when(!recents.is_empty(), |this| {
            this.child(
                div()
                    .id("welcome-recents")
                    .mt_6()
                    .w(px(420.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .px_2()
                            .pb_1()
                            .child(t("Recent", "最近")),
                    )
                    .children(recents.iter().take(6).map(|project| {
                        let project = project.clone();
                        div()
                            .id(format!("recent-{}", element_id(&project)))
                            .group("recent-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .h(px(32.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.secondary_hover))
                            .on_click({
                                let project = project.clone();
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_open_recent(&project, cx);
                                })
                            })
                            .child(
                                Icon::new(IconName::Folder)
                                    .xsmall()
                                    .flex_shrink_0()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .truncate()
                                    .child(project_label(&project)),
                            )
                            .child(hover_delete_button(
                                format!("recent-remove-{}", element_id(&project)),
                                IconName::X,
                                "recent-row",
                                {
                                    let project = project.clone();
                                    cx.listener(move |workspace, _, _, cx| {
                                        workspace.on_remove_recent(&project, cx);
                                    })
                                },
                                cx,
                            ))
                    })),
            )
        })
        .into_any_element()
}

fn welcome_action(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ink = theme.foreground;
    // Outline only: neither action takes a fill; the wordmark carries color.
    skin::glass_button(id, false, theme)
        .text_color(ink)
        .child(Icon::new(icon).with_size(px(15.)).text_color(ink))
        .child(label)
}

/// The desk title, one color per letter along the brand ramp. On mount the
/// ramp sweeps once across the letters and settles; reduced motion paints
/// the resting colors directly.
fn render_wordmark(dark: bool, reduce_motion: bool) -> gpui_kit::AnyElement {
    if reduce_motion {
        return StyledText::new(WORDMARK)
            .with_highlights(wordmark_highlights(dark, 0.))
            .into_any_element();
    }
    StyledText::new(WORDMARK)
        .with_animation(
            "welcome-wordmark",
            Animation::new(std::time::Duration::from_millis(WORDMARK_SWEEP_MS))
                .with_easing(ease_out_quint())
                .with_max_fps(60.),
            move |text, delta| text.with_highlights(wordmark_highlights(dark, 1. - delta)),
        )
        .into_any_element()
}

/// Blue, indigo, violet, pink: the Desk info, accent, and violet hues, with
/// deeper shades in light mode so the letters keep contrast on the page.
fn wordmark_stops(dark: bool) -> [Hsla; 4] {
    let stops: [u32; 4] = if dark {
        [0x60A5FA, 0x7C8CFF, 0xA78BFA, 0xF0ABFC]
    } else {
        [0x2563EB, 0x4353D8, 0x7C3AED, 0xC026D3]
    };
    stops.map(|hex| gpui_kit::rgb(hex).into())
}

/// Color at `t` on the cyclic ramp; `t` wraps, so a sweep never runs off it.
fn wordmark_ramp(stops: &[Hsla; 4], t: f32) -> Hsla {
    let scaled = t.rem_euclid(1.) * stops.len() as f32;
    let index = (scaled.floor() as usize) % stops.len();
    let from = stops[index].to_rgb();
    let to = stops[(index + 1) % stops.len()].to_rgb();
    let mix = scaled.fract();
    gpui_kit::Rgba {
        r: from.r + (to.r - from.r) * mix,
        g: from.g + (to.g - from.g) * mix,
        b: from.b + (to.b - from.b) * mix,
        a: 1.,
    }
    .into()
}

/// Per-letter colors, offset along the ramp by `shift` (0 is at rest).
fn wordmark_highlights(dark: bool, shift: f32) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let stops = wordmark_stops(dark);
    let last = (WORDMARK.chars().count().max(2) - 1) as f32;
    WORDMARK
        .char_indices()
        .enumerate()
        .map(|(index, (start, letter))| {
            let t = index as f32 / last * WORDMARK_SPAN + shift;
            (
                start..start + letter.len_utf8(),
                HighlightStyle {
                    color: Some(wordmark_ramp(&stops, t)),
                    ..HighlightStyle::default()
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        NEW_TASK_LABEL, TAGLINE_HAS_FOLDER, TAGLINE_NO_FOLDER, WORDMARK, empty_desk_tagline,
        wordmark_highlights, wordmark_stops,
    };

    fn rejects_chat_wording(text: &str) {
        assert!(
            !text.to_ascii_lowercase().contains("chat"),
            "empty-desk copy must not say chat: {text}"
        );
        assert!(
            !text.contains("对话"),
            "empty-desk copy must not say chat: {text}"
        );
    }

    #[test]
    fn empty_desk_copy_is_folder_or_task_and_never_chat() {
        for (english, chinese) in [TAGLINE_NO_FOLDER, TAGLINE_HAS_FOLDER, NEW_TASK_LABEL] {
            rejects_chat_wording(english);
            rejects_chat_wording(chinese);
        }
        assert_eq!(empty_desk_tagline(false), TAGLINE_NO_FOLDER);
        assert_eq!(empty_desk_tagline(true), TAGLINE_HAS_FOLDER);
        assert_eq!(TAGLINE_NO_FOLDER.0, "Open a folder to get started.");
        assert_eq!(TAGLINE_HAS_FOLDER.0, "Start a task in this workspace.");
        assert_eq!(NEW_TASK_LABEL.0, "New task");
    }

    #[test]
    fn the_resting_wordmark_starts_on_blue_and_never_goes_grey() {
        for dark in [true, false] {
            let stops = wordmark_stops(dark);
            let runs = wordmark_highlights(dark, 0.);
            assert_eq!(runs.len(), WORDMARK.chars().count());
            let first = runs[0].1.color.expect("first letter color").to_rgb();
            let blue = stops[0].to_rgb();
            assert!((first.r - blue.r).abs() < 1e-3 && (first.b - blue.b).abs() < 1e-3);
            for (_, style) in &runs {
                let color = style.color.expect("letter color");
                assert!(color.s > 0.3, "letters stay saturated, got {color:?}");
            }
            // A whole-ramp shift lands back on the resting colors, so the
            // sweep ends without a jump.
            let swept = wordmark_highlights(dark, 1.);
            for ((_, rest), (_, after)) in runs.iter().zip(&swept) {
                let rest = rest.color.expect("rest").to_rgb();
                let after = after.color.expect("after").to_rgb();
                assert!((rest.g - after.g).abs() < 1e-3);
            }
        }
    }
}
