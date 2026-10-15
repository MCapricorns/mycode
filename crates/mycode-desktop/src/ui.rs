//! GPUI rendering for the workspace: a frosted sidebar, a conversation that
//! fills the space up to Details, and an inspector on the right. The window
//! is dark.
mod chat;
pub(crate) mod command_palette;
mod context;
pub(crate) mod desk;
pub(crate) mod model_picker;
mod motion;
pub(crate) mod project_picker;
mod settings;
mod sidebar;
mod skin;
mod title_bar;
mod update_dialog;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::view_model::MainView;
use crate::workspace::{ToastKind, Workspace};

pub(crate) use chat::composer_placeholder;
pub(crate) use command_palette::ToggleCommandPalette;
pub(crate) use settings::{
    BackendForm, McpForm, ProviderForm, build_custom_backend, build_custom_provider,
    build_mcp_server,
};

/// How much of the desk chrome fits the current window width.
///
/// The inspector starts closed. On a wide window it can be pinned beside the
/// conversation; otherwise it opens as a drawer over the chat.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct DeskLayout {
    /// Whether pinning the inspector still leaves a readable transcript.
    wide: bool,
}

impl DeskLayout {
    /// Width at which a pinned inspector can sit beside the conversation.
    const PIN_MIN: f32 = 1280.;

    fn of(window: &Window) -> Self {
        let width = f32::from(window.viewport_size().width);
        Self {
            wide: width >= Self::PIN_MIN,
        }
    }

    /// Wide enough that a pinned Details column can sit beside the chat.
    pub(super) fn docks_inspector(window: &Window) -> bool {
        Self::of(window).wide
    }
}

/// Renders the whole window chrome and content.
pub fn render_root(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    if let Some(prefer) = motion::system_prefers_reduced_motion() {
        cx.set_reduce_motion(prefer);
    }
    workspace.sync_interface_font(window, cx);
    workspace.sync_localized_placeholders(window, cx);
    let layout = DeskLayout::of(window);
    let inspector_docked = layout.wide
        && workspace.vm().inspector_open
        && workspace.vm().inspector_pinned
        && workspace.vm().view == MainView::Chat;
    let inspector_overlay =
        workspace.vm().inspector_open && !inspector_docked && workspace.vm().view == MainView::Chat;
    let inspector_span = if inspector_docked {
        context::INSPECTOR_DOCKED_WIDTH
    } else if inspector_overlay {
        context::INSPECTOR_OVERLAY_SPAN
    } else {
        0.
    };
    let theme = cx.theme().clone();
    let ui_font = theme.font_family.clone();
    let fg = theme.foreground;
    let focus_handle = workspace.focus_handle().clone();
    // Opening settings unmounts the composer and the sidebar control that
    // was focused. A stale focus handle is not in this frame, so key dispatch
    // never reaches the listeners below. Park focus on the workspace node.
    if workspace.take_settings_focus() {
        focus_handle.focus(window, cx);
    }
    div()
        .id("workspace")
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .text_color(fg)
        // Root focus plus the key listeners below keep Escape and the command
        // palette shortcut alive even when no input holds focus. Capture sees
        // the shortcut before a focused field can take the character. Settings
        // Escape is captured too: the search field and the unmounted composer
        // would otherwise swallow it, and Back to desk advertises Esc.
        // Bubble still sees Escape on the desk after a focused field declines it.
        .track_focus(&focus_handle)
        // No opaque page here. Rails and the reading column paint their own
        // fills so the window blur shows through the chrome.
        .can_drop(|value, _, _| value.is::<gpui_kit::ExternalPaths>())
        .on_drop(
            cx.listener(|workspace, paths: &gpui_kit::ExternalPaths, _, cx| {
                workspace.on_drop_project(paths.paths(), cx);
            }),
        )
        .on_action(
            cx.listener(|workspace, _: &ToggleCommandPalette, window, cx| {
                workspace.toggle_command_palette(window, cx);
            }),
        )
        .capture_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
            if workspace.on_menu_key_down(event, window, cx) {
                cx.stop_propagation();
            } else if command_palette::is_command_palette_shortcut(&event.keystroke) {
                cx.stop_propagation();
                workspace.toggle_command_palette(window, cx);
            } else if event.keystroke.key == "escape"
                && (workspace.vm().command_palette_open
                    || workspace.vm().view == MainView::Settings)
            {
                // Capture, not bubble: a focused settings field declines Escape
                // before the root's bubble listener runs. The palette is the
                // top layer and is closed first inside `on_escape`.
                cx.stop_propagation();
                workspace.on_escape(window, cx);
            }
        }))
        .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                workspace.on_escape(window, cx);
            }
        }))
        .font_family(ui_font)
        .child(title_bar::render_title_bar(workspace, window, cx))
        .child(
            div()
                .id("body")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .flex_row()
                .when(workspace.vm().view == MainView::Chat, |this| {
                    this.child(sidebar::render_sidebar(workspace, cx))
                })
                .child(main_pane(workspace, window, cx))
                .when(inspector_docked, |this| {
                    this.child(context::render_context_panel(workspace, window, cx))
                }),
        )
        .when(workspace.vm().project_menu_open, |this| {
            this.child(sidebar::render_project_menu_layer(workspace, cx))
        })
        .when(workspace.vm().workspace_menu_open, |this| {
            this.child(sidebar::render_workspace_menu_layer(workspace, cx))
        })
        .when(
            workspace.vm().subagent_window.is_some()
                && crate::view_model::task_surface_visible(workspace.vm()),
            |this| this.child(context::render_subagent_window(workspace, cx)),
        )
        .when(inspector_overlay, |this| {
            this.child(context::render_inspector_drawer(workspace, cx))
        })
        // Drawer and diff are painted after the Details overlay. Its backdrop
        // is a full-window click target; if these panels were underneath,
        // the first click on a file would dismiss Details instead of opening
        // the diff.
        .when(workspace.vm().changes_panel_open, |this| {
            this.child(context::render_changes_drawer(
                workspace,
                window,
                inspector_span,
                cx,
            ))
        })
        .when(workspace.git_diff_panel_open(), |this| {
            this.child(context::render_diff_panel(
                workspace,
                window,
                inspector_span,
                cx,
            ))
        })
        .when(workspace.vm().update_dialog_open, |this| {
            this.child(update_dialog::render_update_dialog(workspace, cx))
        })
        .when(workspace.project_picker.is_some(), |this| {
            this.child(project_picker::render(workspace, cx))
        })
        .when(workspace.vm().command_palette_open, |this| {
            this.child(command_palette::render(workspace, window, cx))
        })
        .child(render_toasts(workspace, window, cx))
}

/// Chat or settings, faded in when that pane is entered.
fn main_pane(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let pane_key = match workspace.vm().view {
        MainView::Chat => "chat",
        MainView::Settings => "settings",
    };
    let pane = match workspace.vm().view {
        MainView::Chat => chat::render_chat(workspace, window, cx).into_any_element(),
        MainView::Settings => {
            settings::render_settings_view(workspace, window, cx).into_any_element()
        }
    };
    motion::fade_in(
        format!("main-pane-{pane_key}"),
        cx.reduce_motion(),
        div()
            .id("main-pane")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex()
            .flex_col()
            .child(pane),
    )
}

fn render_toasts(
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let toasts = workspace.toasts();
    div()
        .id("toasts")
        .max_h((window.viewport_size().height - px(100.)).max(px(120.)))
        .overflow_y_scroll()
        .absolute()
        .bottom(px(20.))
        .left_0()
        .right_0()
        .px_4()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .children(toasts.iter().map(|toast| {
            let toast_id = toast.id;
            let (strip, icon) = match toast.kind {
                ToastKind::Error => (theme.danger, IconName::CircleAlert),
                ToastKind::Warning => (theme.warning, IconName::TriangleAlert),
                ToastKind::Info => (theme.info, IconName::Info),
            };
            div()
                .id(format!("toast-{toast_id}"))
                .flex_shrink_0()
                .occlude()
                .w(px(460.))
                .max_w_full()
                .rounded(skin::radius_card())
                .border_1()
                .border_color(skin::border_strong(theme))
                .bg(skin::toast_fill(theme))
                .shadow(skin::elevation_shadow(theme))
                .text_color(theme.foreground)
                .overflow_hidden()
                .flex()
                .flex_row()
                .items_stretch()
                .child(div().w(px(4.)).flex_shrink_0().bg(strip))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_3()
                        .py_2p5()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap_2p5()
                        .child(
                            div()
                                .mt(px(2.))
                                .flex_shrink_0()
                                .child(Icon::new(icon).with_size(px(15.)).text_color(strip)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .whitespace_normal()
                                .child(toast.text.clone()),
                        )
                        .child(
                            div()
                                .id(format!("toast-close-{toast_id}"))
                                .size(px(22.))
                                .rounded(skin::radius_xs())
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_color(theme.muted_foreground)
                                .hover(|this| {
                                    this.bg(theme.secondary_hover).text_color(theme.foreground)
                                })
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    cx.stop_propagation();
                                    workspace.on_dismiss_toast(toast_id, cx);
                                }))
                                .child(Icon::new(IconName::X).xsmall()),
                        ),
                )
        }))
}

/// Compact token-count spelling: 12.3k / 1.2M. Shared by the inspector.
pub(super) fn compact_count(count: u64) -> String {
    crate::view_model::compact_count(count)
}

/// A 7px status lamp.
pub(super) fn lamp(color: gpui_kit::Hsla) -> impl IntoElement {
    div().size(px(7.)).rounded_full().bg(color)
}

pub(super) fn icon_button(
    id: impl Into<gpui_kit::ElementId>,
    icon: IconName,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    icon_button_marked(id, icon, false, on_click, cx)
}

/// Icon button with an optional quiet selected fill. Resting state is muted;
/// the light background appears on hover, or while `marked` is set.
pub(super) fn icon_button_marked(
    id: impl Into<gpui_kit::ElementId>,
    icon: IconName,
    marked: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let hover = theme.secondary_hover;
    div()
        .id(id)
        .size(px(30.))
        .rounded(px(10.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if marked {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .when(marked, |this| this.bg(theme.accent.opacity(0.55)))
        .hover(move |this| this.bg(hover))
        .child(Icon::new(icon).with_size(px(15.)))
        .on_click(on_click)
}

/// A hover-revealed delete affordance: hidden until the parent row's group
/// hovers, so rows stay clean at rest. Used by the welcome recents, the
/// session rows, and the sidebar workspace-root rows.
pub(super) fn hover_delete_button(
    id: impl Into<gpui_kit::ElementId>,
    icon: IconName,
    group: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(20.))
        .rounded(px(2.))
        .flex_shrink_0()
        .opacity(0.0)
        .group_hover(group, |this| this.opacity(1.0))
        .cursor_pointer()
        .text_color(theme.muted_foreground)
        .hover(|this| this.bg(theme.secondary_hover))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .child(Icon::new(icon).xsmall())
}

pub(super) fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// Stable element id for a full path. Prefix truncation collides on siblings.
pub(super) fn element_id(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(super) fn ellipsis(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}\u{2026}", &text[..end]),
        None => text.to_owned(),
    }
}

pub(super) fn project_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    #[test]
    fn previews_truncate_at_character_boundaries_and_keep_exact_length_text() {
        assert_eq!(super::ellipsis("", 0), "");
        assert_eq!(super::ellipsis("abc", 0), "\u{2026}");
        assert_eq!(super::ellipsis("abc", 3), "abc");
        assert_eq!(super::ellipsis("abc", 2), "ab\u{2026}");
        assert_eq!(
            super::ellipsis("\u{4e2d}\u{1f680}tail", 2),
            "\u{4e2d}\u{1f680}\u{2026}"
        );
    }
}
