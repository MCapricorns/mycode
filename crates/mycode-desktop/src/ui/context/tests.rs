use super::inspector_flags_after_pin;

#[test]
fn floating_details_reserves_the_overlay_and_a_docked_panel_does_not() {
    let wide = super::INSPECTOR_OVERLAY_SPAN + super::MIN_CHAT_BESIDE_DETAILS + 80.;
    assert_eq!(super::chat_right_reserve(false, false, wide), 0.);
    assert_eq!(super::chat_right_reserve(false, true, wide), 0.);
    assert_eq!(super::chat_right_reserve(true, true, wide), 0.);
    assert_eq!(
        super::chat_right_reserve(true, false, wide),
        super::INSPECTOR_OVERLAY_SPAN
    );
    assert_eq!(
        super::chat_right_reserve(true, false, super::MIN_CHAT_BESIDE_DETAILS),
        0.,
        "a pane that is already the minimum keeps its width"
    );
    let tight = super::MIN_CHAT_BESIDE_DETAILS + 100.;
    assert_eq!(super::chat_right_reserve(true, false, tight), 100.);
}

#[test]
fn pinning_leaves_the_panel_open() {
    assert_eq!(inspector_flags_after_pin(false), (true, true));
    assert_eq!(inspector_flags_after_pin(true), (true, false));
}

#[test]
fn the_diff_sits_left_of_details_and_the_drawer() {
    let (drawer, diff) = super::panel_rights(0., false);
    assert_eq!(diff, 12.);
    assert_eq!(drawer, 12.);

    assert_eq!(
        super::INSPECTOR_OVERLAY_SPAN,
        super::INSPECTOR_OVERLAY_MARGIN + super::INSPECTOR_OVERLAY_WIDTH
    );
    let (_, beside_details) = super::panel_rights(super::INSPECTOR_OVERLAY_SPAN, false);
    assert_eq!(beside_details, super::INSPECTOR_OVERLAY_SPAN + 12.);

    let (drawer, beside_drawer) = super::panel_rights(super::INSPECTOR_DOCKED_WIDTH, true);
    assert_eq!(drawer, super::INSPECTOR_DOCKED_WIDTH + 12.);
    assert!(beside_drawer > drawer + 480.);
    assert_eq!(
        beside_drawer,
        super::INSPECTOR_DOCKED_WIDTH + 12. + 480. + 12.
    );
}

#[test]
fn the_subagent_window_stacks_left_of_details_drawer_and_diff() {
    let wide = 1520.;
    // Alone: clear of Details by the panel gap.
    let (right, width) =
        super::subagent_window_bounds(wide, super::INSPECTOR_OVERLAY_SPAN, false, false);
    assert_eq!(right, super::INSPECTOR_OVERLAY_SPAN + 12.);
    assert_eq!(width, super::SUBAGENT_WINDOW_WIDTH);
    // With the drawer open it moves left of the drawer.
    let (right, _) =
        super::subagent_window_bounds(wide, super::INSPECTOR_OVERLAY_SPAN, true, false);
    assert!(right > super::INSPECTOR_OVERLAY_SPAN + 480.);
    // With drawer and diff open it moves left of both when the window
    // is wide enough to stack all three.
    let (right, _) =
        super::subagent_window_bounds(1920., super::INSPECTOR_OVERLAY_SPAN, true, true);
    assert!(right > super::INSPECTOR_OVERLAY_SPAN + 480. + 560.);
    // When everything cannot fit, it stays inside the viewport.
    let (right, _) = super::subagent_window_bounds(700., super::INSPECTOR_OVERLAY_SPAN, true, true);
    assert!(right + super::SUBAGENT_WINDOW_WIDTH <= 700.);
}

#[test]
fn context_bar_fraction_is_zero_when_empty_and_proportional_to_window() {
    assert_eq!(super::context_bar_fraction(0, 0), 0.0);
    assert_eq!(super::context_bar_fraction(0, 200_000), 0.0);
    assert!((super::context_bar_fraction(50_000, 200_000) - 0.25).abs() < f32::EPSILON);
    assert!((super::context_bar_fraction(200_000, 200_000) - 1.0).abs() < f32::EPSILON);
    assert!((super::context_bar_fraction(250_000, 200_000) - 1.0).abs() < f32::EPSILON);
}

#[test]
fn responsive_panel_bounds_keep_drawer_and_diff_on_screen_across_window_widths() {
    for width in [960., 1100., 1280., 1360., 1520.] {
        for span in [
            0.,
            super::INSPECTOR_DOCKED_WIDTH,
            super::INSPECTOR_OVERLAY_SPAN,
        ] {
            for drawer_open in [false, true] {
                let ((drawer_right, drawer_w), (diff_right, diff_w)) =
                    super::responsive_panel_bounds(width, span, drawer_open);
                let drawer_left = width - drawer_right - drawer_w;
                let diff_left = width - diff_right - diff_w;
                assert!(
                    drawer_left >= 12. - f32::EPSILON,
                    "drawer left {drawer_left} off-screen at width {width}, span {span}"
                );
                assert!(
                    diff_left >= 12. - f32::EPSILON,
                    "diff left {diff_left} off-screen at width {width}, span {span}, drawer {drawer_open}"
                );
                assert!(drawer_w >= 280.);
                assert!(diff_w >= 280.);
            }
        }
    }
}
