//! Parent-turn resource limits, shared with all delegated work.

use serde::{Deserialize, Serialize};

/// Largest configurable combined model-request allowance for one user turn.
pub const MAX_MODEL_REQUESTS_LIMIT: u32 = 4096;

/// Provider-neutral limits applied when a user turn begins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct RunLimitsSettings {
    /// Maximum agent provider invocations across the parent and its children.
    /// Context overflow retries consume another slot. Provider-internal network
    /// retries and auxiliary compaction/title requests are outside this counter.
    pub max_model_requests: u32,
}

impl Default for RunLimitsSettings {
    fn default() -> Self {
        Self {
            max_model_requests: crate::agent::DEFAULT_MAX_MODEL_REQUESTS,
        }
    }
}

impl RunLimitsSettings {
    /// Whether a model-request allowance is within the supported range.
    pub fn is_valid_limit(value: u32) -> bool {
        (1..=MAX_MODEL_REQUESTS_LIMIT).contains(&value)
    }
}

pub(super) fn run_limits_are_default(limits: &RunLimitsSettings) -> bool {
    limits == &RunLimitsSettings::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppSettings, salvage_app_settings};
    use serde_json::json;

    #[test]
    fn existing_documents_keep_defaults_and_limits_validate_at_both_edges() {
        let defaults: AppSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(defaults.run_limits.max_model_requests, 64);
        for limit in [1, MAX_MODEL_REQUESTS_LIMIT] {
            let settings: AppSettings =
                serde_json::from_value(json!({"runLimits": {"maxModelRequests": limit}})).unwrap();
            settings.validate().unwrap();
            let encoded = serde_json::to_value(&settings).unwrap();
            assert_eq!(encoded["runLimits"]["maxModelRequests"], limit);
        }
        for limit in [0, MAX_MODEL_REQUESTS_LIMIT + 1] {
            let settings: AppSettings =
                serde_json::from_value(json!({"runLimits": {"maxModelRequests": limit}})).unwrap();
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn repair_preserves_valid_limits_and_resets_only_invalid_limits() {
        let valid = salvage_app_settings(&json!({
            "runLimits": {"maxModelRequests": 7},
            "appearance": {"theme": "invalid"}
        }))
        .unwrap();
        assert_eq!(valid.run_limits.max_model_requests, 7);
        let invalid = salvage_app_settings(&json!({
            "runLimits": {"maxModelRequests": 0},
            "appearance": {"theme": "light"}
        }))
        .unwrap();
        assert_eq!(invalid.run_limits, RunLimitsSettings::default());
        assert_eq!(invalid.appearance.theme, "light");
    }
}
