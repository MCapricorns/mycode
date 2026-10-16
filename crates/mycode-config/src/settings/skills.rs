//! Skill enablement persisted in `settings.json`.
//!
//! Skills are discovered from `.agents` trees; this document only records
//! the slugs the user switched off. A disabled skill stays listed on the
//! settings page but is left out of system prompts and the composer.

use serde::{Deserialize, Serialize};

use super::{AppSettings, bounded_text};
use crate::ConfigError;

/// Maximum number of disabled skill slugs kept in the document.
pub const MAX_DISABLED_SKILLS: usize = 256;
/// Maximum UTF-8 length of one stored slug.
const MAX_SKILL_SLUG_BYTES: usize = 128;

/// Skill preferences.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillsSettings {
    /// Slugs (without the leading `/`) of skills the user turned off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
}

impl SkillsSettings {
    /// Returns whether a skill slug is enabled. Unlisted skills are enabled.
    #[must_use]
    pub fn is_enabled(&self, slug: &str) -> bool {
        !self.disabled.iter().any(|disabled| disabled == slug)
    }

    /// Turns one skill on or off. Returns whether the list changed.
    /// Disabling stops at [`MAX_DISABLED_SKILLS`].
    pub fn set_enabled(&mut self, slug: &str, enabled: bool) -> bool {
        let before = self.disabled.len();
        if enabled {
            self.disabled.retain(|disabled| disabled != slug);
        } else if self.is_enabled(slug) && before < MAX_DISABLED_SKILLS {
            self.disabled.push(slug.to_owned());
        }
        self.disabled.len() != before
    }
}

pub(super) fn skills_are_default(skills: &SkillsSettings) -> bool {
    skills.disabled.is_empty()
}

impl AppSettings {
    /// Validates the disabled-skill list: entry count, slug bounds, and
    /// duplicates.
    pub(super) fn validate_skills(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        if self.skills.disabled.len() > MAX_DISABLED_SKILLS {
            return Err(invalid("skills.disabled: too many entries"));
        }
        for (index, slug) in self.skills.disabled.iter().enumerate() {
            let field = format!("skills.disabled[{index}]");
            if slug.trim().is_empty() || bounded_text(slug, MAX_SKILL_SLUG_BYTES).is_err() {
                return Err(invalid(&format!(
                    "{field}: must be 1-{MAX_SKILL_SLUG_BYTES} bytes without line breaks or NUL"
                )));
            }
            if self.skills.disabled[..index].contains(slug) {
                return Err(invalid(&format!("{field}: duplicates an earlier entry")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{AppSettings, SkillsSettings};

    #[test]
    fn toggling_a_skill_round_trips_and_default_is_omitted() {
        let mut settings = AppSettings::default();
        let empty = serde_json::to_value(&settings).expect("serialize");
        assert!(empty.get("skills").is_none(), "{empty}");

        assert!(settings.skills.set_enabled("review", false));
        assert!(!settings.skills.set_enabled("review", false));
        assert!(!settings.skills.is_enabled("review"));
        assert!(settings.skills.is_enabled("other"));
        settings.validate().expect("valid");

        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(
            json.contains(r#""skills":{"disabled":["review"]}"#),
            "{json}"
        );
        let parsed: AppSettings = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.skills, settings.skills);

        assert!(settings.skills.set_enabled("review", true));
        assert_eq!(settings.skills, SkillsSettings::default());
    }

    #[test]
    fn duplicate_or_blank_disabled_slugs_are_rejected() {
        let mut settings = AppSettings::default();
        settings.skills.disabled = vec!["a".to_owned(), "a".to_owned()];
        assert!(settings.validate().is_err());
        settings.skills.disabled = vec![" ".to_owned()];
        assert!(settings.validate().is_err());
        settings.skills.disabled = vec!["a\nb".to_owned()];
        assert!(settings.validate().is_err());
    }
}
