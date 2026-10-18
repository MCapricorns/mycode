//! Appearance normalization shared by every frontend.

/// Canonical appearance id. Unknown values fall back to dark.
#[must_use]
pub fn normalize_theme(theme: &str) -> &'static str {
    mycode_config::VALID_THEMES
        .into_iter()
        .find(|id| *id == theme)
        .unwrap_or("dark")
}

/// Canonical font-size id. Unknown values fall back to medium.
#[must_use]
pub fn normalize_font_size(id: &str) -> &'static str {
    mycode_config::VALID_FONT_SIZES
        .into_iter()
        .find(|item| *item == id)
        .unwrap_or("m")
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_themes_fall_back_to_dark() {
        assert_eq!(super::normalize_theme("light"), "light");
        assert_eq!(super::normalize_theme("nope"), "dark");
    }

    #[test]
    fn unknown_font_sizes_fall_back_to_medium() {
        assert_eq!(super::normalize_font_size("xl"), "xl");
        assert_eq!(super::normalize_font_size("huge"), "m");
    }
}
