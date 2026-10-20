//! Fenced-code colors. The lexer is `mycode_frontend::highlight`; this maps
//! its token kinds onto Desk colors and installs the result as the app-wide
//! TextView code-block highlighter, so every reply, user bubble, and
//! subagent transcript colors code the same way.
use gpui_kit::base::TextViewDefaults;
use gpui_kit::base::text::CodeBlock;
use gpui_kit::component::theme::Theme;
use gpui_kit::{App, FontStyle, HighlightStyle, Hsla};
use mycode_frontend::highlight::{TokenKind, highlight};

/// Rebuilds the Base theme, then installs the code highlighter on top.
///
/// `Theme::sync_base` reinstalls the TextView defaults from scratch, which
/// drops a highlighter installed earlier, so every sync goes through here.
/// The new closure also tells each code block its old highlights are stale,
/// so a mode switch repaints code in the new palette.
pub(crate) fn sync_base(cx: &mut App) {
    Theme::sync_base(cx);
    let light = !Theme::global(cx).mode.is_dark();
    TextViewDefaults::global(cx)
        .with_code_block_highlighter(move |block: &CodeBlock| {
            let code = block.code();
            let lang = block.lang();
            highlight(&code, lang.as_deref())
                .into_iter()
                .map(|(range, kind)| (range, style(kind, light)))
                .collect()
        })
        .install(cx);
}

pub(crate) fn style(kind: TokenKind, light: bool) -> HighlightStyle {
    HighlightStyle {
        color: Some(color(kind, light)),
        font_style: (kind == TokenKind::Comment).then_some(FontStyle::Italic),
        ..Default::default()
    }
}

/// Token ink on the code-block fill (`theme.muted`). Dark mode uses soft
/// pastels so code does not glare beside the reply text; light mode uses
/// the 700-weight hues that hold 4.5:1 on the light card.
pub(super) fn color(kind: TokenKind, light: bool) -> Hsla {
    use TokenKind::*;
    let hex = match (kind, light) {
        (Keyword, false) => 0xC49BFF,
        (Keyword, true) => 0x7C3AED,
        (Type | Variable, false) => 0x6FD1E0,
        (Type | Variable, true) => 0x0E7490,
        (Function | Meta, false) => 0x82AAFF,
        (Function | Meta, true) => 0x1D4ED8,
        (String, false) => 0xA8D88A,
        (String, true) => 0x15803D,
        (Number, false) => 0xF5A86B,
        (Number, true) => 0xC2410C,
        (Comment, false) => 0x7F8592,
        (Comment, true) => 0x6B7280,
        (Property, false) => 0xE5C07B,
        (Property, true) => 0x92400E,
        (Macro | Tag, false) => 0xEF9AA5,
        (Macro | Tag, true) => 0xBE185D,
        (Inserted, false) => 0x7FD69B,
        (Inserted, true) => 0x15803D,
        (Deleted, false) => 0xF28B8B,
        (Deleted, true) => 0xB91C1C,
    };
    gpui_kit::rgb(hex).into()
}

/// Every kind, for palette checks.
#[cfg(test)]
pub(super) const KINDS: [TokenKind; 13] = [
    TokenKind::Keyword,
    TokenKind::Type,
    TokenKind::Function,
    TokenKind::String,
    TokenKind::Number,
    TokenKind::Comment,
    TokenKind::Property,
    TokenKind::Macro,
    TokenKind::Variable,
    TokenKind::Tag,
    TokenKind::Inserted,
    TokenKind::Deleted,
    TokenKind::Meta,
];
