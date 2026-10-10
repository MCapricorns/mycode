//! Painted title bar.
//!
//! The system title bar is hidden when the window opens. This strip is the
//! only chrome: it is draggable, a double-click on the empty area toggles
//! zoom, and minimize / zoom / close sit on the right.
//!
//! On Windows the first `window_control_area` hitbox that contains the
//! pointer wins, and a parent `Drag` region is inserted before its children.
//! Caption buttons nested in that region are `HTCAPTION`, so the click never
//! reaches them. The drag region and the caption buttons are siblings: only
//! the title strip is `Drag`. Windows `zoom()` only maximizes, so the max
//! button uses `WindowControlArea::Max` and lets the caption proc toggle.
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
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

pub(super) const BAR_HEIGHT: f32 = 34.;
/// Caption hit target. The bar is taller than this; each button is at least
/// this wide and fills the bar vertically.
const CONTROL_HIT: f32 = 28.;
/// Space between minimize, zoom, and close.
const CONTROL_GAP: f32 = 2.;
/// Inset from the window edge so Linux CSD's resize band does not cover close.
const CAPTION_EDGE: f32 = 4.;
const TITLE_INSET: f32 = 12.;

struct CaptionButton {
    id: &'static str,
    icon: IconName,
    area: WindowControlArea,
    close: bool,
}

/// Right-side order on every desktop: minimize, zoom, close.
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
    let theme = cx.theme().clone();
    let subtitle: SharedString = match workspace.vm().view {
        MainView::Chat => workspace
            .vm()
            .project_dir
            .as_deref()
            .map(super::project_label)
            .unwrap_or_else(|| t("No folder", "未打开目录").to_owned())
            .into(),
        MainView::Settings => t("Settings", "设置").into(),
    };
    // The chip tracks the whole self-update pipeline: offering, downloading,
    // or staged. Its click always opens the update dialog.
    let update_label: Option<SharedString> = match &workspace.vm().update {
        UpdateState::Available { version, .. } => {
            Some(format!("v{version} {}", t("available", "可用")).into())
        }
        UpdateState::Downloading { version } => {
            Some(format!("v{version} {}", t("downloading…", "下载中…")).into())
        }
        UpdateState::Ready { version } => {
            Some(format!("v{version} {}", t("ready", "已就绪")).into())
        }
        _ => None,
    };
    div()
        .id("title-bar")
        .flex()
        .flex_row()
        .items_center()
        .h(px(BAR_HEIGHT))
        .w_full()
        .flex_shrink_0()
        .border_b_1()
        .border_color(skin::glass_border(&theme))
        .bg(skin::glass(&theme))
        .child(drag_region(subtitle, window, cx))
        .child(title_controls(
            update_label,
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
    // double-click-zooms the window. macOS and Linux own that gesture.
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
        .pl(px(TITLE_INSET))
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
                .child(img("brand/icon.ico").size(px(16.)))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::BOLD)
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

fn title_controls(
    update_label: Option<SharedString>,
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
        .pr(px(CAPTION_EDGE))
        // The drag strip is a sibling, and a normal hitbox does not block it.
        // On Windows the first `window_control_area` that contains the pointer
        // wins, so a Drag region under this strip becomes HTCAPTION. On Linux
        // the same overlap arms `start_window_move`, and the client-side
        // resize band is an ancestor `on_mouse_down`. Either one takes the
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
        .when_some(update_label, |this, label| {
            this.child(
                Button::new("title-update")
                    .label(label)
                    .small()
                    .warning()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.apply_action(
                            crate::view_model::DesktopAction::UpdateDialogToggled(true),
                            cx,
                        );
                    })),
            )
        })
        .child(window_controls(window, cx))
}

/// Panel toggle in the title strip.
///
/// Caption buttons already consume the press. This button has to do the same,
/// or Linux delivers the press to the drag region or the resize backdrop and
/// the click never completes. GPUI registers `on_mouse_down` listeners before
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

fn window_controls(window: &mut Window, cx: &mut Context<Workspace>) -> impl IntoElement {
    if cfg!(target_family = "wasm") {
        return div().id("window-controls");
    }
    // A compositor that refuses client decorations still paints its own
    // min/max/close. Skip ours so the two sets are not stacked.
    #[cfg(target_os = "linux")]
    if !matches!(
        window.window_decorations(),
        gpui_kit::Decorations::Client { .. }
    ) {
        return div().id("window-controls");
    }
    let theme = cx.theme().clone();
    let [minimize, zoom, close] = caption_buttons(window.is_maximized());
    div()
        .id("window-controls")
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .flex_shrink_0()
        .gap(px(CONTROL_GAP))
        .child(caption_button(
            minimize.id,
            minimize.icon,
            minimize.area,
            minimize.close,
            &theme,
        ))
        .child(caption_button(
            zoom.id, zoom.icon, zoom.area, zoom.close, &theme,
        ))
        .child(caption_button(
            close.id,
            close.icon,
            close.area,
            close.close,
            &theme,
        ))
}

fn caption_button(
    id: &'static str,
    icon: IconName,
    area: WindowControlArea,
    close: bool,
    theme: &gpui_kit::component::theme::Theme,
) -> impl IntoElement {
    let hover_bg = if close {
        theme.danger
    } else {
        theme.secondary_hover
    };
    let hover_fg = if close {
        theme.danger_foreground
    } else {
        theme.secondary_foreground
    };
    div()
        .id(id)
        .w(px(CONTROL_HIT))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .text_color(theme.foreground)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .when(cfg!(target_os = "windows"), |this| {
            this.window_control_area(area)
        })
        .when(cfg!(not(target_os = "windows")), |this| {
            this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                match area {
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Max => window.zoom_window(),
                    WindowControlArea::Close => window.remove_window(),
                    WindowControlArea::Drag => {}
                }
            })
        })
        .child(Icon::new(icon).small())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_buttons_are_minimize_zoom_close_on_the_right() {
        const _: () = assert!(CONTROL_HIT >= 28.);
        const _: () = assert!(CONTROL_GAP > 0. && CONTROL_GAP <= 4.);
        const _: () = assert!(CAPTION_EDGE >= 4.);
        const _: () = assert!(BAR_HEIGHT >= CONTROL_HIT);
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
            let theme = cx.theme().clone();
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
                        .child(super::inspector_toggle_button(false, &theme, {
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
