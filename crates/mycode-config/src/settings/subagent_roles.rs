//! Subagent delegation settings family.

use serde::{Deserialize, Serialize};

use super::{AppSettings, MAX_FIELD_BYTES, bounded_text};
use crate::ConfigError;

/// Maximum configured subagent roles.
pub const MAX_SUBAGENT_ROLES: usize = 32;
/// Upper bound on the explicit subagent concurrency setting.
pub const MAX_SUBAGENT_CONCURRENCY: u32 = 6;
/// Slot count used when `max_concurrent` is `0`.
///
/// Zero selects this default. It does not mean "run no subagents".
pub const DEFAULT_SUBAGENT_CONCURRENCY: u32 = 4;

/// One role's model route and reasoning override.
///
/// Absent fields mean "inherit": the role runs on the session's own provider
/// and model, at the reasoning level its definition declares. A route names a
/// configured provider id, so it survives a catalog refresh.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentRoleSettings {
    /// Role name, matching a catalog role.
    pub role: String,
    /// Whether the parent model may delegate to this role.
    pub enabled: bool,
    /// Provider id this role runs on; absent inherits the session provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id this role runs on; absent inherits the session model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort override: `default`, `low`, `medium`, or `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// Subagent delegation settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentSettings {
    /// Per-role configuration. A catalog role with no entry here is enabled
    /// and fully inherited, so a fresh install has a working team.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<SubagentRoleSettings>,
    /// Simultaneous subagent limit.
    ///
    /// `0` uses [`DEFAULT_SUBAGENT_CONCURRENCY`]. It does not disable
    /// delegation or mean zero running agents.
    #[serde(default)]
    pub max_concurrent: u32,
}

impl SubagentSettings {
    /// Returns the stored entry for one role, if any.
    #[must_use]
    pub fn role(&self, name: &str) -> Option<&SubagentRoleSettings> {
        self.roles.iter().find(|entry| entry.role == name)
    }

    /// Whether the parent model may delegate to one role.
    ///
    /// Roles are opt-out: a role with no stored entry is available.
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        self.role(name).is_none_or(|entry| entry.enabled)
    }

    /// How many subagents may run at once.
    ///
    /// A stored `0` resolves to [`DEFAULT_SUBAGENT_CONCURRENCY`].
    #[must_use]
    pub fn effective_concurrency(&self) -> u32 {
        if self.max_concurrent == 0 {
            DEFAULT_SUBAGENT_CONCURRENCY
        } else {
            self.max_concurrent
        }
    }

    /// Returns the mutable entry for one role, inserting an inherited default.
    pub fn role_mut(&mut self, name: &str) -> &mut SubagentRoleSettings {
        if let Some(index) = self.roles.iter().position(|entry| entry.role == name) {
            return &mut self.roles[index];
        }
        self.roles.push(SubagentRoleSettings {
            role: name.to_owned(),
            enabled: true,
            provider: None,
            model: None,
            thinking: None,
        });
        self.roles.last_mut().expect("just pushed")
    }
}

pub(super) fn subagents_are_default(subagents: &SubagentSettings) -> bool {
    *subagents == SubagentSettings::default()
}

impl AppSettings {
    /// Validates the subagent family: concurrency bounds, entry bounds, role
    /// grammar, duplicate roles, resolvable model routes, and thinking
    /// vocabulary.
    pub(super) fn validate_subagent_roles(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        if self.subagents.max_concurrent > MAX_SUBAGENT_CONCURRENCY {
            return Err(invalid(&format!(
                "subagents.maxConcurrent: must be 0 (the default of {DEFAULT_SUBAGENT_CONCURRENCY}, not zero agents) through {MAX_SUBAGENT_CONCURRENCY}"
            )));
        }
        if self.subagents.roles.len() > MAX_SUBAGENT_ROLES {
            return Err(invalid("subagents.roles: too many entries"));
        }
        for (index, entry) in self.subagents.roles.iter().enumerate() {
            let field = format!("subagents.roles[{index}]");
            if !crate::home::is_portable_role_name(&entry.role) {
                return Err(invalid(&format!(
                    "{field}.role: must be 1-64 lowercase letters, digits, dash, dot, or underscore"
                )));
            }
            if self.subagents.roles[..index]
                .iter()
                .any(|earlier| earlier.role == entry.role)
            {
                return Err(invalid(&format!(
                    "{field}.role: duplicates an earlier role entry"
                )));
            }
            // A model route without its provider cannot be resolved, and a
            // provider route with no model would silently pick a default the
            // settings page never showed.
            match (entry.provider.as_deref(), entry.model.as_deref()) {
                (Some(provider), Some(model)) => {
                    if !self
                        .providers
                        .iter()
                        .any(|configured| configured.id == provider)
                    {
                        return Err(invalid(&format!(
                            "{field}.provider: \"{provider}\" is not a configured provider"
                        )));
                    }
                    bounded_text(model, MAX_FIELD_BYTES).map_err(|_| {
                        invalid(&format!(
                            "{field}.model: too long or contains control characters"
                        ))
                    })?;
                }
                (None, None) => {}
                _ => {
                    return Err(invalid(&format!(
                        "{field}: set provider and model together, or neither to inherit the session model"
                    )));
                }
            }
            if let Some(level) = entry.thinking.as_deref()
                && crate::RoleThinking::parse(level).is_none()
            {
                return Err(invalid(&format!(
                    "{field}.thinking: must be a models.dev option (default, off, on, minimal, low, medium, high, xhigh, max)"
                )));
            }
        }
        Ok(())
    }
}
