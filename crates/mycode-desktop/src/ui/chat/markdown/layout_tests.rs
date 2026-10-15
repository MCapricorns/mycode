use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AppContext as _, Context, FontWeight, IntoElement, Render, SharedString, TestAppContext,
    TextRun, Window, div, px, size,
};

struct MetricsProbe;

impl Render for MetricsProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[gpui_kit::test]
fn table_columns_follow_font_scale_and_native_glyph_widths(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(800.), px(400.)), |_, _| MetricsProbe);
    cx.update_window(handle.into(), |_, window, cx| {
        let text = "A long proportional-font cell with WWW and iii";
        let rows = vec![vec!["Header".into()], vec![text.into()]];
        let mut theme = cx.theme().clone();
        theme.font_size = px(crate::ui::desk::interface_rem_px("s"));
        let small = super::column_widths(&rows, &theme, window.text_system());
        theme.font_size = px(crate::ui::desk::interface_rem_px("xl"));
        let large = super::column_widths(&rows, &theme, window.text_system());
        assert!(
            large[0] > small[0] + 20.,
            "table widths must follow the interface text scale: {small:?} -> {large:?}"
        );

        let text = "\u{4e2d}\u{6587}\u{5b57}\u{4f53}".repeat(8);
        let rows = vec![vec!["Header".into()], vec![format!("**`{text}`**")]];
        let widths = super::column_widths(&rows, &theme, window.text_system());
        let mut font = gpui_kit::font(theme.mono_font_family.clone());
        font.weight = FontWeight::BOLD;
        let line = window.text_system().shape_line(
            SharedString::from(text.clone()),
            px(crate::ui::desk::interface_font_px("xl")),
            &[TextRun {
                len: text.len(),
                font,
                ..Default::default()
            }],
            None,
        );
        let required = f32::from(line.width()) + 6. + 28.;
        assert!(
            widths[0] >= required,
            "CJK glyphs, bold weight, and inline-code padding must fit: {} < {required}",
            widths[0]
        );
    })
    .unwrap();
}
