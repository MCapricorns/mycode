//! Backs up an unreadable owned document and publishes a canonical default.
//!
//! Startup reads of `settings.json`, `secrets.json`, and `ui.json` use this
//! so a corrupt file cannot blank the window. The previous bytes stay beside
//! the original as `{name}.broken-<nanos>`. Session ledgers are not documents
//! this module handles.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};
use crate::{ConfigError, HomeLayout};

/// A startup document that could not be parsed or validated.
///
/// The previous bytes were copied to [`Self::backup`]. The caller replaces
/// the original path with that document's defaults. Session ledgers are
/// never reported here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentRepair {
    /// Owned relative path that was reset, such as `settings.json`.
    pub path: &'static str,
    /// Owned relative path of the preserved bytes.
    pub backup: String,
}

pub(crate) struct Loaded<T> {
    pub value: T,
    pub repair: Option<DocumentRepair>,
}

/// Largest corrupt document this module will copy aside.
///
/// A file above the normal read cap but within this bound is still preserved
/// and replaced. Anything larger is left untouched so a failed read cannot
/// delete bytes that were never copied.
const QUARANTINE_MAX_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn is_document_damage(error: &ConfigError) -> bool {
    matches!(
        error.kind(),
        ConfigErrorKind::InvalidJson
            | ConfigErrorKind::AuthorityValidation
            | ConfigErrorKind::NonUtf8
            | ConfigErrorKind::Oversized
    )
}

/// Copies `bytes` to `{relative}.broken-<nanos>` and leaves the original file
/// in place.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::PathEscape`] when `relative` is not a single
/// safe file name, and [`ConfigError`] when the backup cannot be published.
pub fn quarantine_owned_bytes(
    home: &HomeLayout,
    relative: &'static str,
    bytes: &[u8],
) -> Result<DocumentRepair, ConfigError> {
    if relative.is_empty() || relative.contains(['/', '\\']) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let backup = format!("{relative}.broken-{stamp}");
    if backup.len() > 200 || backup.ends_with(['.', ' ']) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    let payload = bytes.to_vec();
    let limit = payload.len().max(1);
    locked_update_owned_file(home, &backup, limit, |_| Ok(payload))?;
    Ok(DocumentRepair {
        path: relative,
        backup,
    })
}

pub(crate) fn load_or_reset<T>(
    home: &HomeLayout,
    relative: &'static str,
    max_bytes: usize,
    decode: impl FnOnce(&[u8]) -> Result<T, ConfigError>,
    fallback: impl FnOnce() -> T,
    publish: impl FnOnce() -> Result<(), ConfigError>,
) -> Result<Loaded<T>, ConfigError> {
    match read_owned_file(home, relative, max_bytes) {
        Ok(None) => Ok(Loaded {
            value: fallback(),
            repair: None,
        }),
        Ok(Some(bytes)) => match decode(bytes.as_slice()) {
            Ok(value) => Ok(Loaded {
                value,
                repair: None,
            }),
            Err(error) if is_document_damage(&error) => {
                reset_from_bytes(home, relative, bytes.as_slice(), fallback, publish)
            }
            Err(error) => Err(error),
        },
        Err(error) if error.kind() == ConfigErrorKind::Oversized => {
            match read_owned_file(home, relative, QUARANTINE_MAX_BYTES.max(max_bytes)) {
                Ok(Some(bytes)) => {
                    reset_from_bytes(home, relative, bytes.as_slice(), fallback, publish)
                }
                Ok(None) => Ok(Loaded {
                    value: fallback(),
                    repair: None,
                }),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn reset_from_bytes<T>(
    home: &HomeLayout,
    relative: &'static str,
    bytes: &[u8],
    fallback: impl FnOnce() -> T,
    publish: impl FnOnce() -> Result<(), ConfigError>,
) -> Result<Loaded<T>, ConfigError> {
    let repair = quarantine_owned_bytes(home, relative, bytes)?;
    publish()?;
    Ok(Loaded {
        value: fallback(),
        repair: Some(repair),
    })
}

#[cfg(test)]
mod tests {
    use super::quarantine_owned_bytes;
    use crate::{
        HomeLayout, read_app_settings_with_repair, read_provider_secrets_with_repair,
        read_ui_state_with_repair, replace_app_settings,
    };

    fn home(label: &str) -> (tempfile_guard::Guard, HomeLayout) {
        let parent = std::env::temp_dir().join(format!(
            "mycode-repair-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let root = parent.join("home");
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let layout = HomeLayout::from_root(&root).unwrap();
        (tempfile_guard::Guard(parent), layout)
    }

    fn write_private(path: &std::path::Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    mod tempfile_guard {
        pub struct Guard(pub std::path::PathBuf);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn damaged_settings_ui_and_secrets_reset_without_dropping_the_backup() {
        let (_guard, home) = home("docs");
        write_private(&home.root().join("settings.json"), b"{not-json");
        write_private(&home.root().join("ui.json"), b"{\"formatVersion\":");
        write_private(&home.root().join("secrets.json"), b"\xff\xfe broken");

        let (settings, settings_repair) = read_app_settings_with_repair(&home).unwrap();
        assert_eq!(settings.appearance.palette, "slate");
        assert_eq!(settings.appearance.theme, "dark");
        let settings_repair = settings_repair.expect("settings repair");
        assert_eq!(settings_repair.path, "settings.json");
        let settings_backup = std::fs::read(home.root().join(&settings_repair.backup)).unwrap();
        assert_eq!(settings_backup, b"{not-json");
        let again = read_app_settings_with_repair(&home).unwrap();
        assert!(
            again.1.is_none(),
            "a repaired settings file must stay readable"
        );

        let (ui, ui_repair) = read_ui_state_with_repair(&home).unwrap();
        assert!(ui.auto_update);
        assert!(ui.workspaces.is_empty());
        let ui_repair = ui_repair.expect("ui repair");
        assert!(
            std::fs::read(home.root().join(&ui_repair.backup))
                .unwrap()
                .starts_with(b"{\"formatVersion\":")
        );

        let (secrets, secrets_repair) = read_provider_secrets_with_repair(&home).unwrap();
        assert!(secrets.provider_ids().is_empty());
        let secrets_repair = secrets_repair.expect("secrets repair");
        assert_eq!(
            std::fs::read(home.root().join(&secrets_repair.backup)).unwrap(),
            b"\xff\xfe broken"
        );
    }

    #[test]
    fn invalid_palette_resets_settings_and_keeps_the_old_bytes() {
        let (_guard, home) = home("palette");
        let bytes = br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 3,
            "appearance": { "theme": "dark", "palette": "nope", "language": "zh" },
            "providers": [{
                "id": "openai",
                "kind": "openai-completions",
                "baseUrl": "https://api.openai.com/v1",
                "models": ["gpt-4o"]
            }]
        }"#;
        write_private(&home.root().join("settings.json"), bytes);
        let (settings, repair) = read_app_settings_with_repair(&home).unwrap();
        assert!(settings.providers.is_empty());
        assert_eq!(settings.appearance.palette, "slate");
        let repair = repair.expect("validation repair");
        let backup = std::fs::read(home.root().join(&repair.backup)).unwrap();
        assert!(
            backup.windows(4).any(|window| window == b"nope"),
            "backup lost"
        );
        assert!(backup.windows(6).any(|window| window == b"openai"));
    }

    #[test]
    fn valid_settings_and_trailing_commas_are_not_quarantined() {
        let (_guard, home) = home("valid");
        let mut settings = crate::AppSettings::default();
        settings.appearance.palette = "ocean".to_owned();
        replace_app_settings(&home, crate::AuthorityRevision::ABSENT, &settings).unwrap();
        let (loaded, repair) = read_app_settings_with_repair(&home).unwrap();
        assert!(repair.is_none());
        assert_eq!(loaded.appearance.palette, "ocean");

        write_private(
            &home.root().join("ui.json"),
            br#"{"formatVersion":1,"kind":"mycode-ui-state","autoUpdate":false,}"#,
        );
        let (ui, repair) = read_ui_state_with_repair(&home).unwrap();
        assert!(repair.is_none(), "trailing comma is an in-place repair");
        assert!(!ui.auto_update);
        assert!(
            !home
                .root()
                .read_dir()
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains(".broken-"))
        );
    }

    #[test]
    fn quarantine_rejects_a_nested_path() {
        let (_guard, home) = home("path");
        let error = quarantine_owned_bytes(&home, "sessions/note.json", b"{}").unwrap_err();
        assert_eq!(error.kind(), crate::ConfigErrorKind::PathEscape);
    }
}
