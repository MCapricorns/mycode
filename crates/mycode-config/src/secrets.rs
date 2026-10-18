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
const SECRETS_KIND: &str = "mycode-provider-secrets";
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
            .binary_search_by(|(id, _)| id.as_str().cmp(provider_id))
            .ok()
            .map(|index| self.provider_keys[index].1.as_str())
    }

    /// Sets or clears one provider key; sorted by provider id.
    #[must_use]
    pub fn with_key(mut self, provider_id: &str, key: Option<&str>) -> Self {
        let key = key.filter(|key| !key.is_empty());
        match (
            self.provider_keys
                .binary_search_by(|(id, _)| id.as_str().cmp(provider_id)),
            key,
        ) {
            (Ok(index), Some(key)) => self.provider_keys[index].1 = key.to_owned(),
            (Ok(index), None) => {
                self.provider_keys.remove(index);
            }
            (Err(index), Some(key)) => {
                self.provider_keys
                    .insert(index, (provider_id.to_owned(), key.to_owned()));
            }
            (Err(_), None) => {}
        }
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
/// A document that cannot be parsed or validated is backed up and replaced:
/// whatever keys still decode and validate are kept so a corrupt entry does
/// not force every provider to be re-keyed. The backup keeps the old bytes;
/// this function never logs them.
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
/// store was republished.
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
        |bytes| Ok(decode_secrets(bytes)?.secrets),
        ProviderSecrets::new,
        salvage_provider_secrets,
        |secrets| publish_secrets(home, secrets),
    )?;
    Ok((loaded.value, loaded.repair))
}

fn publish_secrets(home: &HomeLayout, secrets: &ProviderSecrets) -> Result<(), ConfigError> {
    secrets.validate()?;
    let bytes = encode_secrets(secrets, AuthorityRevision::ABSENT.checked_next()?)?;
    locked_update_owned_file(home, SECRETS_PATH, MAX_SECRETS_BYTES, |_| Ok(bytes))
}

/// Serializes one validated store at `revision`.
fn encode_secrets(
    secrets: &ProviderSecrets,
    revision: AuthorityRevision,
) -> Result<Vec<u8>, ConfigError> {
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
    Ok(bytes)
}

/// Extracts the still-valid keys of a document the strict read rejected, so
/// a reset keeps every credential that is intact. Entries with an invalid
/// provider id or an unusable key are dropped. `None` when there is no
/// `providerKeys` object to read.
pub fn salvage_provider_secrets(document: &serde_json::Value) -> Option<ProviderSecrets> {
    let keys = document.as_object()?.get("providerKeys")?.as_object()?;
    let mut secrets = ProviderSecrets::new();
    for (id, key) in keys {
        let Some(key) = key.as_str() else { continue };
        if !crate::home::is_valid_portable_id(id) {
            continue;
        }
        secrets = secrets.with_key(id, Some(key));
    }
    secrets.validate().ok()?;
    Some(secrets)
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
        let bytes = encode_secrets(secrets, revision)?;
        published_revision = Some(revision);
        Ok(bytes)
    })?;
    published_revision.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

/// Sets or clears one provider key in a single locked read-modify-write.
///
/// Unlike [`replace_provider_secrets`], two writers that race never fail
/// each other with a revision conflict: each applies its own key to the
/// document the lock hands it, so concurrent key saves (an `mcp.json`
/// import, an OAuth token rotation mid-turn) all land. Returns the store as
/// published.
///
/// # Errors
///
/// Returns [`ConfigError`] when the stored document fails validation, the
/// updated store is invalid, or the transaction fails.
pub fn update_provider_key(
    home: &HomeLayout,
    provider_id: &str,
    key: Option<&str>,
) -> Result<ProviderSecrets, ConfigError> {
    let mut published = None;
    locked_update_owned_file(home, SECRETS_PATH, MAX_SECRETS_BYTES, |current| {
        let (secrets, current_revision) = match current {
            Some(bytes) => {
                let parsed = decode_secrets(bytes)?;
                (parsed.secrets, parsed.revision)
            }
            None => (ProviderSecrets::new(), AuthorityRevision::ABSENT),
        };
        let secrets = secrets.with_key(provider_id, key);
        secrets.validate()?;
        let bytes = encode_secrets(&secrets, current_revision.checked_next()?)?;
        published = Some(secrets);
        Ok(bytes)
    })?;
    published.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
}

fn parse_document_header(bytes: &[u8]) -> Result<AuthorityRevision, ConfigError> {
    Ok(decode_secrets(bytes)?.revision)
}

fn decode_secrets(bytes: &[u8]) -> Result<ParsedSecrets, ConfigError> {
    let document = serde_json::from_slice::<DeserializedSecrets>(bytes)
        .map_err(|_| ConfigError::authority_rejection())?;
    if document.format_version != SECRETS_FORMAT_VERSION || document.kind != SECRETS_KIND {
        return Err(ConfigError::authority_rejection()
            .with_detail("secrets.json: formatVersion or kind does not match this build"));
    }
    let revision = AuthorityRevision::new(document.revision)?;
    let secrets = ProviderSecrets {
        provider_keys: document
            .provider_keys
            .into_iter()
            .filter(|(_, key)| !key.is_empty())
            .collect(),
    };
    secrets.validate()?;
    Ok(ParsedSecrets { secrets, revision })
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_secrets_field_is_rejected() {
        let bytes = br#"{
            "formatVersion": 1,
            "kind": "mycode-provider-secrets",
            "revision": 1,
            "providerKeys": {},
            "providers": { "openai": "sk-test" }
        }"#;
        let error = super::decode_secrets(bytes)
            .err()
            .expect("unknown field in secrets.json must fail validation");
        assert_eq!(error.kind(), crate::ConfigErrorKind::AuthorityValidation);
    }

    #[test]
    fn concurrent_key_updates_all_land() {
        let parent = std::env::temp_dir().join(format!(
            "mycode-secrets-race-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&parent).expect("temp parent");
        let root = parent.join("home");
        std::fs::create_dir(&root).expect("home");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("private home");
        }
        let home = crate::HomeLayout::from_root(&root).expect("layout");
        // Each writer used to read, then CAS against the revision it read;
        // every racer but one failed with a conflict and its key was lost.
        let writers: Vec<_> = (0..6)
            .map(|index| {
                let home = home.clone();
                std::thread::spawn(move || {
                    super::update_provider_key(
                        &home,
                        &format!("mcp-server{index}"),
                        Some(&format!("sk-{index}")),
                    )
                    .expect("update");
                })
            })
            .collect();
        for writer in writers {
            writer.join().expect("writer");
        }
        let secrets = super::read_provider_secrets(&home).expect("read");
        for index in 0..6 {
            assert_eq!(
                secrets.key(&format!("mcp-server{index}")),
                Some(format!("sk-{index}").as_str())
            );
        }
        // Clearing goes through the same locked edit.
        let cleared = super::update_provider_key(&home, "mcp-server0", None).expect("clear");
        assert!(cleared.key("mcp-server0").is_none());
        assert_eq!(cleared.provider_ids().len(), 5);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn a_malformed_secrets_document_resets_and_salvages_intact_keys() {
        let parent = std::env::temp_dir().join(format!(
            "mycode-secrets-comma-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&parent).expect("temp parent");
        let root = parent.join("home");
        std::fs::create_dir(&root).expect("home");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("private home");
        }
        let home = crate::HomeLayout::from_root(&root).expect("layout");
        // Valid JSON that the strict decode rejects (a non-string key and a
        // retired field) plus two healthy entries: the read resets the file
        // but keeps the keys that still decode and validate.
        let body = br#"{"formatVersion":1,"kind":"mycode-provider-secrets","revision":3,"providers":{},"providerKeys":{"openai":"sk-ok","bad id!":"sk-drop","zai":7}}"#;
        // The fixture goes through the secure transaction: a plain fs::write
        // file inherits the temp directory's DACL and the Windows read path
        // rejects it before the revision check this test exercises.
        crate::write_owned_test_bytes(&home, "secrets.json", body).expect("write");
        let (secrets, repair) =
            super::read_provider_secrets_with_repair(&home).expect("read resets");
        assert!(repair.is_some(), "the malformed file was reported");
        assert_eq!(secrets.provider_ids(), ["openai"]);
        assert_eq!(secrets.key("openai"), Some("sk-ok"));
        let republished = std::fs::read(root.join("secrets.json")).expect("republished");
        let text = String::from_utf8(republished).expect("utf-8");
        assert!(!text.contains("zai"), "canonical document: {text}");
        assert!(text.contains("\"openai\""), "{text}");
        assert!(super::read_provider_secrets(&home).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::remove_dir_all(&parent);
    }
}
