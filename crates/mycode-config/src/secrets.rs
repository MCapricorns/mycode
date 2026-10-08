//! Strict provider secret store: `secrets.json`.
//!
//! API keys never enter `settings.json`. This document holds one bounded key
//! per configured provider id under the same owned-file transaction and
//! revision CAS discipline as settings. Debug output is redacted so keys can
//! never leak into logs.

use serde::Deserialize;

use crate::authority::AuthorityRevision;
use crate::secure_fs::owned_file::locked_update_owned_file;
use crate::{ConfigError, ConfigErrorKind, HomeLayout};

/// Exact secrets document path.
pub const SECRETS_PATH: &str = "secrets.json";
/// Maximum encoded size of the secrets document.
pub const MAX_SECRETS_BYTES: usize = 64 * 1024;
/// Exact secrets format version.
pub const SECRETS_FORMAT_VERSION: u32 = 1;
/// Exact secrets document kind.
pub const SECRETS_KIND: &str = "mycode-provider-secrets";
/// Maximum providers with stored keys.
pub(crate) const MAX_SECRET_PROVIDERS: usize = 64;
/// Maximum accepted key length in bytes.
pub(crate) const MAX_KEY_BYTES: usize = 16 * 1024;

/// One key per provider id; serialized as a sorted object map.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ProviderSecrets {
    provider_keys: Vec<(String, String)>,
}

impl ProviderSecrets {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the stored key for one provider.
    #[must_use]
    pub fn key(&self, provider_id: &str) -> Option<&str> {
        self.provider_keys
            .iter()
            .find(|(id, _)| id == provider_id)
            .map(|(_, key)| key.as_str())
    }

    /// Sets or clears one provider key; sorted by provider id.
    #[must_use]
    pub fn with_key(mut self, provider_id: &str, key: Option<&str>) -> Self {
        self.provider_keys.retain(|(id, _)| id != provider_id);
        if let Some(key) = key.filter(|key| !key.is_empty()) {
            self.provider_keys
                .push((provider_id.to_owned(), key.to_owned()));
        }
        self.provider_keys.sort_by(|a, b| a.0.cmp(&b.0));
        self
    }

    /// Lists stored provider ids without exposing keys.
    #[must_use]
    pub fn provider_ids(&self) -> Vec<&str> {
        self.provider_keys
            .iter()
            .map(|(id, _)| id.as_str())
            .collect()
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.provider_keys.len() > MAX_SECRET_PROVIDERS {
            return Err(ConfigError::authority_rejection());
        }
        for (id, key) in &self.provider_keys {
            if !crate::home::is_valid_portable_id(id)
                || key.is_empty()
                || key.len() > MAX_KEY_BYTES
                || key.contains(['\0', '\r', '\n'])
            {
                return Err(ConfigError::authority_rejection());
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for ProviderSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.provider_ids()).finish()
    }
}

/// Reads `secrets.json`; a missing document yields an empty store.
///
/// A recoverable older copy (trailing commas) is rewritten in canonical form.
/// A document that cannot be parsed or validated is backed up and replaced
/// with an empty store so startup can continue. The backup keeps the old
/// bytes; this function never logs them.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or when a damaged document
/// cannot be copied aside.
pub fn read_provider_secrets(home: &HomeLayout) -> Result<ProviderSecrets, ConfigError> {
    Ok(read_provider_secrets_with_repair(home)?.0)
}

/// Reads `secrets.json`, repairing a damaged document.
///
/// The second value is set when the previous bytes were copied aside and the
/// store was reset to empty.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or when the backup or the
/// replacement cannot be published.
pub fn read_provider_secrets_with_repair(
    home: &HomeLayout,
) -> Result<(ProviderSecrets, Option<crate::DocumentRepair>), ConfigError> {
    let loaded = crate::document_repair::load_or_reset(
        home,
        SECRETS_PATH,
        MAX_SECRETS_BYTES,
        |bytes| {
            let parsed = decode_secrets(bytes)?;
            if parsed.migrated {
                let _ = replace_provider_secrets(home, parsed.revision, &parsed.secrets);
            }
            Ok(parsed.secrets)
        },
        ProviderSecrets::new,
        || publish_default_secrets(home),
    )?;
    Ok((loaded.value, loaded.repair))
}

fn publish_default_secrets(home: &HomeLayout) -> Result<(), ConfigError> {
    let revision = AuthorityRevision::ABSENT.checked_next()?;
    let mut document = serde_json::Map::new();
    document.insert("formatVersion".into(), SECRETS_FORMAT_VERSION.into());
    document.insert("kind".into(), SECRETS_KIND.into());
    document.insert("revision".into(), revision.get().into());
    document.insert("providerKeys".into(), serde_json::Map::new().into());
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    bytes.push(b'\n');
    locked_update_owned_file(home, SECRETS_PATH, MAX_SECRETS_BYTES, |_| Ok(bytes))
}

/// Replaces `secrets.json` under revision compare-and-swap.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::RevisionConflict`] for a stale expectation and
/// [`ConfigError`] for validation or transaction failures.
pub fn replace_provider_secrets(
    home: &HomeLayout,
    expected_revision: AuthorityRevision,
    secrets: &ProviderSecrets,
) -> Result<AuthorityRevision, ConfigError> {
    secrets.validate()?;
    let mut published_revision = None;
    locked_update_owned_file(home, SECRETS_PATH, MAX_SECRETS_BYTES, |current| {
        let current_revision = match current {
            Some(bytes) => parse_document_header(bytes)?,
            None => AuthorityRevision::ABSENT,
        };
        if current_revision != expected_revision {
            return Err(ConfigError::new(ConfigErrorKind::RevisionConflict));
        }
        let revision = current_revision.checked_next()?;
        let mut document = serde_json::Map::new();
        document.insert("formatVersion".into(), SECRETS_FORMAT_VERSION.into());
        document.insert("kind".into(), SECRETS_KIND.into());
        document.insert("revision".into(), revision.get().into());
        let mut keys = serde_json::Map::new();
        for (id, key) in &secrets.provider_keys {
            keys.insert(id.clone(), key.clone().into());
        }
        document.insert("providerKeys".into(), keys.into());
        let mut bytes = serde_json::to_vec_pretty(&document)
            .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_SECRETS_BYTES {
            return Err(ConfigError::new(ConfigErrorKind::Oversized));
        }
        published_revision = Some(revision);
        Ok(bytes)
    })?;
    published_revision.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedSecrets {
    format_version: u32,
    kind: String,
    revision: u64,
    #[serde(default)]
    provider_keys: std::collections::BTreeMap<String, String>,
}

struct ParsedSecrets {
    secrets: ProviderSecrets,
    revision: AuthorityRevision,
    migrated: bool,
}

fn parse_document_header(bytes: &[u8]) -> Result<AuthorityRevision, ConfigError> {
    Ok(decode_secrets(bytes)?.revision)
}

fn decode_secrets(bytes: &[u8]) -> Result<ParsedSecrets, ConfigError> {
    let decoded = crate::json_recover::decode_json::<DeserializedSecrets>(bytes)?;
    let document = decoded.value;
    if document.format_version != SECRETS_FORMAT_VERSION || document.kind != SECRETS_KIND {
        return Err(ConfigError::authority_rejection()
            .with_detail("secrets.json: formatVersion or kind does not match this build"));
    }
    let revision = AuthorityRevision::new(document.revision)?;
    let secrets = document
        .provider_keys
        .into_iter()
        .fold(ProviderSecrets::new(), |secrets, (id, key)| {
            secrets.with_key(&id, Some(&key))
        });
    secrets.validate()?;
    Ok(ParsedSecrets {
        secrets,
        revision,
        migrated: decoded.migrated,
    })
}
