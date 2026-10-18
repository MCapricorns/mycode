//! Appearance normalization shared by every frontend.

/// Canonical appearance id. Unknown values fall back to dark.
#[must_use]
pub fn normalize_theme(theme: &str) -> &'static str {
    mycode_config::VALID_THEMES
        .into_iter()
        .find(|id| *id == theme)
        .unwrap_or("dark")
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_themes_fall_back_to_dark() {
        assert_eq!(super::normalize_theme("light"), "light");
        assert_eq!(super::normalize_theme("nope"), "dark");
    }
}
