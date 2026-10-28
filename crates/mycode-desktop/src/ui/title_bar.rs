//! Painted title bar.
//!
//! The system title bar is hidden when the window opens. This strip is the
//! only chrome: it is draggable, a double-click on the empty area toggles
//! zoom, and on Windows minimize / zoom / close sit on the right. macOS keeps
//! its native traffic lights.
//!
//! On Windows the first `window_control_area` hitbox that contains the
//! pointer wins, and a parent `Drag` region is inserted before its children.
//! Caption buttons nested in that region are `HTCAPTION`, so the click never
//! reaches them. The drag region and the caption buttons are siblings: only
//! the title strip is `Drag`. Windows `zoom()` only maximizes, so the max
//! button uses `WindowControlArea::Max` and lets the caption proc toggle.
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, InteractiveElementExt as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowControlArea, div, img, px,
};

use super::skin;
use crate::i18n::t;
use crate::view_model::{MainView, UpdateState};
use crate::workspace::Workspace;

pub(super) const BAR_HEIGHT: f32 = 40.;
/// Caption hit target on Windows.
const CONTROL_HIT: f32 = 46.;
/// Fixed caption glyph size. These icons are window chrome, so they stay a
/// constant pixel size instead of tracking the UI font scale.
const CAPTION_ICON_PX: f32 = 12.;
/// The native Windows close-hover red and the white glyph it carries.
const CLOSE_HOVER_BG: u32 = 0xe81123;
const CLOSE_HOVER_FG: u32 = 0xffffff;
const TITLE_INSET: f32 = 12.;
/// Leading space reserved on macOS for native traffic-light window controls.
const MACOS_TRAFFIC_LIGHT_INSET: f32 = 76.;

#[must_use]
const fn title_leading_inset(is_macos: bool) -> f32 {
    if is_macos {
        MACOS_TRAFFIC_LIGHT_INSET
    } else {
        TITLE_INSET
    }
}

struct CaptionButton {
    id: &'static str,
    icon: IconName,
    area: WindowControlArea,
    close: bool,
}

/// Right-side order: minimize, zoom, close.
fn caption_buttons(maximized: bool) -> [CaptionButton; 3] {
    [
        CaptionButton {
            id: "minimize",
            icon: IconName::WindowMinimize,
            area: WindowControlArea::Min,
            close: false,
        },
        if maximized {
            CaptionButton {
                id: "restore",
                icon: IconName::WindowRestore,
                area: WindowControlArea::Max,
                close: false,
            }
        } else {
            CaptionButton {
                id: "maximize",
                icon: IconName::WindowMaximize,
                area: WindowControlArea::Max,
                close: false,
            }
        },
        CaptionButton {
            id: "close",
            icon: IconName::WindowClose,
            area: WindowControlArea::Close,
            close: true,
        },
    ]
}

pub(super) fn render_title_bar(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let subtitle: SharedString = match workspace.vm().view {
        MainView::Chat => workspace
            .vm()
            .project_dir
            .as_deref()
            .map(super::project_label)
            .map(|project| format!("{} / {project}", t("Workbench", "工作台")))
            .unwrap_or_else(|| t("Local workbench", "本地代码工作台").to_owned())
            .into(),
        MainView::Settings => t("Settings", "设置").into(),
    };
    // A small dot tracks the self-update pipeline. Its click opens the dialog.
    let update_dot = update_dot_color(&workspace.vm().update, theme);
    div()
        .id("title-bar")
        .flex()
        .flex_row()
        .items_center()
        .h(px(BAR_HEIGHT))
        .w_full()
        .flex_shrink_0()
        .bg(skin::glass(theme))
        .child(drag_region(subtitle, window, cx))
        .child(title_controls(
            update_dot,
            workspace.vm().view == MainView::Chat,
            workspace.vm().inspector_open,
            window,
            cx,
        ))
}

fn drag_region(
    subtitle: SharedString,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    // Windows treats `Drag` as HTCAPTION, which already moves and
    // double-click-zooms the window. On macOS the app owns that gesture.
    let state = window.use_state(cx, |_, _| TitleDrag { armed: false });
    div()
        .id("title-drag")
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .h_full()
        .min_w_0()
        .flex_1()
        .pl(px(title_leading_inset(cfg!(target_os = "macos"))))
        .window_control_area(WindowControlArea::Drag)
        .when(cfg!(not(target_os = "windows")), |this| {
            this.on_double_click(|_, window, _| {
                window.zoom_window();
            })
            .on_mouse_down_out(window.listener_for(&state, |state, _, _, _| {
                state.armed = false;
            }))
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(&state, |state, _, _, _| {
                    state.armed = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                window.listener_for(&state, |state, _, _, _| {
                    state.armed = false;
                }),
            )
            .on_mouse_move(window.listener_for(&state, |state, _, window, _| {
                if state.armed {
                    state.armed = false;
                    window.start_window_move();
                }
            }))
        })
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(img("brand/icon.ico").size(px(22.)))
                .child(
                    div()
                        .text_sm()
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                        .text_color(theme.foreground)
                        .child("MYCode"),
                ),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(subtitle),
        )
}

struct TitleDrag {
    armed: bool,
}

impl Render for TitleDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn update_dot_color(
    state: &UpdateState,
    theme: &gpui_kit::component::theme::Theme,
) -> Option<gpui_kit::Hsla> {
    match state {
        UpdateState::Available { .. } => Some(theme.primary),
        UpdateState::Downloading { .. } => Some(theme.primary),
        UpdateState::Ready { .. } => Some(theme.green),
        _ => None,
    }
}

fn title_controls(
    update_dot: Option<gpui_kit::Hsla>,
    show_inspector: bool,
    inspector_open: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    div()
        .id("title-controls")
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .flex_shrink_0()
        .gap_2()
        // Windows caption buttons sit flush on the window's right edge;
        // other platforms keep the usual inset on this strip.
        .when(cfg!(not(target_os = "windows")), |this| {
            this.pr(px(TITLE_INSET))
        })
        // The drag strip is a sibling, and a normal hitbox does not block it.
        // On Windows the first `window_control_area` that contains the pointer
        // wins, so a Drag region under this strip becomes HTCAPTION. On macOS
        // the same overlap arms `start_window_move`. Either one takes the
        // pointer before mouse-up, so a plain `on_click` never opens the
        // inspector. Occlude drops the drag region out of the hit test.
        .occlude()
        .when(show_inspector, |this| {
            this.child(inspector_toggle_button(
                inspector_open,
                cx.theme(),
                cx.listener(|workspace, _, _, cx| {
                    let open = !workspace.vm().inspector_open;
                    let pinned = workspace.vm().inspector_pinned;
                    workspace.on_set_inspector(open, pinned, cx);
                }),
            ))
        })
        .when_some(update_dot, |this, color| {
            let hover = cx.theme().secondary_hover;
            this.child(
                div()
                    .id("title-update")
                    .occlude()
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .rounded(skin::radius_xs())
                    .hover(move |this| this.bg(hover))
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.apply_action(
                            crate::view_model::FrontendAction::UpdateDialogToggled(true),
                            cx,
                        );
                    }))
                    .child(div().size(skin::space(2)).rounded_full().bg(color)),
            )
        })
        // macOS paints native traffic lights; only Windows gets captions.
        .when(cfg!(target_os = "windows"), |this| {
            this.child(window_controls(window, cx))
        })
}

/// Panel toggle in the title strip.
///
/// Caption buttons already consume the press. This button has to do the same,
/// or macOS delivers the press to the drag region's ancestors and the click
/// never completes. GPUI registers `on_mouse_down` listeners before
/// the click's internal press handler and then runs the bubble phase in
/// reverse, so the click arms and this guard then stops the ancestor.
fn inspector_toggle_button(
    open: bool,
    theme: &gpui_kit::component::theme::Theme,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> impl IntoElement {
    let hover = theme.secondary_hover;
    div()
        .id("toggle-inspector")
        .occlude()
        .size(px(30.))
        .rounded(px(10.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if open {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .when(open, |this| this.bg(theme.accent.opacity(0.55)))
        .hover(move |this| this.bg(hover))
        .when(cfg!(not(target_os = "windows")), |this| {
            this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
        })
        .on_click(on_click)
        .child(Icon::new(IconName::PanelRight).with_size(px(15.)))
        .test_support()
}

/// Windows caption buttons: contiguous and flush on the window's right
/// edge, like the native chrome.
fn window_controls(window: &mut Window, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let [minimize, zoom, close] = caption_buttons(window.is_maximized());
    div()
        .id("window-controls")
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .flex_shrink_0()
        .child(caption_button(
            minimize.id,
            minimize.icon,
            minimize.area,
            minimize.close,
            theme,
        ))
        .child(caption_button(
            zoom.id, zoom.icon, zoom.area, zoom.close, theme,
        ))
        .child(caption_button(
            close.id,
            close.icon,
            close.area,
            close.close,
            theme,
        ))
}

/// Hover background and glyph color for a caption button. Close paints the
/// Windows red with a white glyph; the others take a quiet foreground wash.
fn caption_hover_colors(
    close: bool,
    theme: &gpui_kit::component::theme::Theme,
) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    if close {
        (
            gpui_kit::rgb(CLOSE_HOVER_BG).into(),
            gpui_kit::rgb(CLOSE_HOVER_FG).into(),
        )
    } else {
        (theme.foreground.opacity(0.08), theme.foreground)
    }
}

fn caption_button(
    id: &'static str,
    icon: IconName,
    area: WindowControlArea,
    close: bool,
    theme: &gpui_kit::component::theme::Theme,
) -> impl IntoElement {
    let (hover_bg, hover_fg) = caption_hover_colors(close, theme);
    div()
        .id(id)
        .w(px(CONTROL_HIT))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .text_color(theme.muted_foreground)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .window_control_area(area)
        .child(Icon::new(icon).with_size(px(CAPTION_ICON_PX)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_buttons_are_minimize_zoom_close_on_the_right() {
        const _: () = assert!(BAR_HEIGHT == 40.);
        const _: () = assert!(CONTROL_HIT >= 40.);
        // The glyph is a fixed pixel size around the native caption icon.
        const _: () = assert!(CAPTION_ICON_PX >= 10. && CAPTION_ICON_PX <= 14.);
        const _: () = assert!(CLOSE_HOVER_BG == 0xe81123);
        const _: () = assert!(CLOSE_HOVER_FG == 0xffffff);
        assert_eq!(title_leading_inset(false), TITLE_INSET);
        assert_eq!(title_leading_inset(true), MACOS_TRAFFIC_LIGHT_INSET);
        assert!(title_leading_inset(true) > title_leading_inset(false));
        let normal: Vec<_> = caption_buttons(false)
            .into_iter()
            .map(|button| button.id)
            .collect();
        assert_eq!(normal, ["minimize", "maximize", "close"]);
        let zoomed: Vec<_> = caption_buttons(true)
            .into_iter()
            .map(|button| button.id)
            .collect();
        assert_eq!(zoomed, ["minimize", "restore", "close"]);
        assert!(caption_buttons(false).iter().all(|button| {
            matches!(
                button.area,
                WindowControlArea::Min | WindowControlArea::Max | WindowControlArea::Close
            )
        }));
    }

    #[test]
    fn caption_hover_is_windows_red_only_for_close() {
        let theme = gpui_kit::component::theme::Theme::default();
        let (bg, fg) = caption_hover_colors(true, &theme);
        assert_eq!(bg, gpui_kit::rgb(CLOSE_HOVER_BG).into());
        assert_eq!(fg, gpui_kit::rgb(CLOSE_HOVER_FG).into());
        let (bg, fg) = caption_hover_colors(false, &theme);
        assert_eq!(bg, theme.foreground.opacity(0.08));
        assert_eq!(fg, theme.foreground);
    }
}

#[cfg(all(test, feature = "test-support"))]
mod hit_test {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui_kit::component::ActiveTheme as _;
    use gpui_kit::test::{TestSupportExt as _, TestWindowExt};
    use gpui_kit::{
        AppContext as _, Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
        Render, Styled, TestAppContext, Window, div, px, size,
    };

    #[derive(Clone, Copy, Default)]
    struct Presses {
        opened: bool,
        drag_press: bool,
        ancestor_press: bool,
    }

    /// The toggle sits on top of a full-size drag target, under a parent that
    /// also sees presses. A click has to open the panel without either of
    /// those handlers running.
    struct InspectorProbe {
        presses: Rc<Cell<Presses>>,
    }

    impl Render for InspectorProbe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme();
            let presses = self.presses.clone();
            let drag = presses.clone();
            let ancestor = presses.clone();
            div()
                .id("backdrop")
                .size_full()
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    let mut presses = ancestor.get();
                    presses.ancestor_press = true;
                    ancestor.set(presses);
                })
                .child(
                    div()
                        .id("title-drag")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(MouseButton::Left, move |_, _, _| {
                            let mut presses = drag.get();
                            presses.drag_press = true;
                            drag.set(presses);
                        })
                        .test_support(),
                )
                .child(
                    div()
                        .id("title-controls")
                        .absolute()
                        .top_0()
                        .right_0()
                        .w(px(40.))
                        .h(px(34.))
                        .occlude()
                        .child(super::inspector_toggle_button(false, theme, {
                            let presses = presses.clone();
                            move |_, _, _| {
                                let mut flags = presses.get();
                                flags.opened = true;
                                presses.set(flags);
                            }
                        })),
                )
        }
    }

    #[gpui_kit::test]
    fn inspector_toggle_click_does_not_fall_through(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let presses = Rc::new(Cell::new(Presses::default()));
        let flags = presses.clone();
        let handle = cx.open_window(size(px(800.), px(200.)), move |_, _| InspectorProbe {
            presses: flags,
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let button = window.find("toggle-inspector").bounds();
            let drag = window.find("title-drag").bounds();
            assert!(button.size.width > px(0.) && button.size.height > px(0.));
            assert!(
                drag.origin.x <= button.origin.x
                    && drag.origin.y <= button.origin.y
                    && drag.right() >= button.right()
                    && drag.bottom() >= button.bottom(),
                "the drag region should cover the button so the test can see a fall-through: button {button:?} drag {drag:?}"
            );
            window.click("toggle-inspector", cx);
        })
        .unwrap();
        let presses = presses.get();
        assert!(presses.opened);
        assert!(!presses.drag_press);
        assert!(!presses.ancestor_press);
    }
}
