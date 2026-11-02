use super::*;
use gpui_kit::component::theme::Theme;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Render, TestAppContext, size};

struct RolesProbe {
    wide: bool,
    font_size: &'static str,
}

impl Render for RolesProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::desk::apply_font_size(Theme::global_mut(cx), self.font_size);
        window.set_rem_size(px(crate::ui::desk::interface_rem_px(self.font_size)));
        let theme = cx.theme();
        div()
            .id("roles-list")
            .w_full()
            .flex()
            .flex_col()
            .test_support()
            .children(mycode::config::builtin_roles().roles.iter().map(|role| {
                role_settings_row(
                    role,
                    RoleControls {
                        model: role_field_button(
                            &role.name,
                            "model",
                            "codex · a-model-name-that-must-stay-inside-its-column",
                            false,
                            theme,
                        )
                        .into_any_element(),
                        thinking: role_field_button(
                            &role.name,
                            "thinking",
                            "Default · High",
                            false,
                            theme,
                        )
                        .into_any_element(),
                        toggle: Switch::new(format!("toggle-{}", role.name))
                            .checked(true)
                            .accessibility_label(format!("Enable {}", role.name))
                            .into_any_element(),
                    },
                    None,
                    self.wide,
                    theme,
                )
            }))
    }
}

#[gpui_kit::test]
fn four_roles_stay_compact_and_controls_fit_at_both_widths(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for (width, wide) in [(640., false), (912., true)] {
        for font_size in ["m", "xl"] {
            let handle = cx.open_window(size(px(width), px(800.)), move |_, _| RolesProbe {
                wide,
                font_size,
            });
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                let first_top = window.find("agent-role-scout").bounds().top();
                let last_bottom = window.find("agent-role-skeptic").bounds().bottom();
                assert!(
                    last_bottom - first_top < px(600.),
                    "four roles take too much height: {:?}",
                    last_bottom - first_top
                );
                let mut previous_bottom = px(0.);
                for name in ["scout", "artisan", "sentinel", "skeptic"] {
                    let row = window.find(format!("agent-role-{name}")).bounds();
                    let summary = window.find(format!("agent-summary-{name}")).bounds();
                    let controls = window.find(format!("agent-controls-{name}")).bounds();
                    assert!(row.top() >= previous_bottom && row.right() <= px(width));
                    assert!(controls.left() >= row.left() && controls.right() <= row.right());
                    assert!(controls.bottom() <= row.bottom());
                    if wide {
                        assert!(summary.right() <= controls.left());
                    } else {
                        assert!(summary.bottom() <= controls.top());
                    }
                    previous_bottom = row.bottom();
                }
            })
            .unwrap();
        }
    }
}
