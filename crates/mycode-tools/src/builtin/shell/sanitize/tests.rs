use super::sanitize_captured_shell_text;

#[test]
fn clixml_x001b_sgr_fragment_is_plain_text() {
    let raw = "_x001B_[31;1mGet-ChildItem: _x001B_[0m";
    let cleaned = sanitize_captured_shell_text(raw);
    assert_eq!(cleaned, "Get-ChildItem: ");
    assert!(!cleaned.contains("_x001B_"), "{cleaned:?}");
    assert!(!cleaned.contains('\u{1b}'), "{cleaned:?}");
    assert!(!cleaned.contains("[31"), "{cleaned:?}");
    assert!(!cleaned.contains("[0m"), "{cleaned:?}");
}

#[test]
fn raw_esc_csi_sequence_is_stripped() {
    let raw = "\u{1b}[31;1merror\u{1b}[0m";
    assert_eq!(sanitize_captured_shell_text(raw), "error");
}

#[test]
fn clixml_error_node_strips_embedded_ansi() {
    let raw = "\
#< CLIXML
<Objs Version=\"1.1.0.1\" xmlns=\"http://schemas.microsoft.com/powershell/2004/04\"><S S=\"Error\">_x001B_[31;1mGet-ChildItem: _x001B_[0mmissing_x000D__x000A_</S></Objs>";
    let cleaned = sanitize_captured_shell_text(raw);
    assert_eq!(cleaned, "Get-ChildItem: missing");
    assert!(!cleaned.contains("_x001B_"), "{cleaned:?}");
    assert!(!cleaned.contains('\u{1b}'), "{cleaned:?}");
    assert!(!cleaned.contains("#< CLIXML"), "{cleaned:?}");
}

#[test]
fn ordinary_text_and_cr_lf_tab_decoding_still_work() {
    assert_eq!(sanitize_captured_shell_text("plain text"), "plain text");
    assert_eq!(
        sanitize_captured_shell_text("keep trailing\n"),
        "keep trailing\n"
    );
    assert_eq!(
        sanitize_captured_shell_text("line1_x000D__x000A_line2_x0009_end"),
        "line1\r\nline2\tend"
    );
    let blob = "#< CLIXML\n<Objs><S S=\"Error\">left_x000D__x000A_right &amp; &lt;x&gt;</S><S S=\"Warning\">careful</S></Objs>";
    assert_eq!(
        sanitize_captured_shell_text(blob),
        "left\r\nright & <x>\ncareful"
    );
    let untouched = "before\n#< CLIXML\n<Objs><S S=\"Information\">nope</S></Objs>\nafter";
    assert_eq!(sanitize_captured_shell_text(untouched), "before\nafter");
}

#[test]
fn osc_fe_and_invalid_scalars_are_handled() {
    assert_eq!(
        sanitize_captured_shell_text("\u{1b}]0;title\u{7}name"),
        "name"
    );
    assert_eq!(
        sanitize_captured_shell_text("\u{1b}]8;;https://example.test\u{1b}\\link"),
        "link"
    );
    assert_eq!(sanitize_captured_shell_text("\u{1b}(Bplain"), "plain");
    assert_eq!(sanitize_captured_shell_text("a_xD800_b_xDFFF_c"), "abc");
    assert_eq!(
        sanitize_captured_shell_text(
            "\u{1b}[32mbefore\u{1b}[0m\n#< CLIXML\n<Objs></Objs>\n\u{1b}[31mafter\u{1b}[0m"
        ),
        "before\nafter"
    );
}
