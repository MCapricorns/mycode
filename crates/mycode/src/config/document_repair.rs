//! Backs up an unreadable owned document and republishes it canonical.
//!
//! Startup reads of `settings.json`, `secrets.json`, and `ui.json` use this
//! so a broken file cannot blank the window: damaged content, and a file the
//! strict checks reject for its own permissions, owner, or type, are both
//! moved aside and replaced. A damaged document's replacement keeps every
//! field that still decodes and validates (`salvage`), so one bad section does
//! not force the user to re-enter the rest. Safe Windows DACL drift from
//! same-user external writers is normalized before decoding, without a
//! repair report. The previous file stays beside the original as
//! `{name}.broken-<nanos>`. Session ledgers are not documents this module
//! handles.

use std::io;

use crate::config::ConfigErrorKind;
use crate::config::secure_fs::owned_file::{
    locked_update_owned_file, quarantine_rejected_owned_file, read_owned_file,
    read_quarantined_backup,
};
use crate::config::{ConfigError, HomeLayout};

/// A startup document that could not be read, parsed, or validated.
///
/// The previous bytes sit at [`Self::backup`]. The document at the original
/// path has already been republished from the salvage result or the defaults.
/// Session ledgers are never reported here.
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
        ConfigErrorKind::AuthorityValidation | ConfigErrorKind::Oversized
    )
}

/// Whether a failed read points at the document file itself — permissions,
/// owner, a link, or the wrong file type — rather than a passing I/O hiccup.
///
/// A file copied from another machine, or one whose ACL was stripped,
/// failed here with `owned path access control failed` on every launch.
fn is_unusable_file(error: &ConfigError) -> bool {
    match error.kind() {
        ConfigErrorKind::AccessControl => {
            matches!(
                error.io_kind(),
                None | Some(io::ErrorKind::PermissionDenied)
            )
        }
        ConfigErrorKind::LinkEscape => true,
        // Native regular-file checks report these for a directory or another
        // wrong type. Other I/O errors do not establish document damage.
        ConfigErrorKind::Io => matches!(
            error.io_kind(),
            Some(io::ErrorKind::InvalidData | io::ErrorKind::IsADirectory)
        ),
        _ => false,
    }
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
    let limit = bytes.len().max(1);
    let backup = publish_unique_backup(relative, crate::core::unix_now().as_nanos(), |backup| {
        locked_update_owned_file(home, backup, limit, |current| {
            if current.is_some() {
                return Err(ConfigError::new(ConfigErrorKind::Io)
                    .with_io_kind(io::ErrorKind::AlreadyExists));
            }
            Ok(bytes.to_vec())
        })
    })?;
    Ok(DocumentRepair {
        path: relative,
        backup,
    })
}

fn publish_unique_backup(
    relative: &str,
    stamp: u128,
    mut publish: impl FnMut(&str) -> Result<(), ConfigError>,
) -> Result<String, ConfigError> {
    for attempt in 0..16u128 {
        let backup = backup_name_with_stamp(relative, stamp.saturating_add(attempt))?;
        match publish(&backup) {
            Ok(()) => return Ok(backup),
            Err(error)
                if error.io_kind() == Some(io::ErrorKind::AlreadyExists)
                    || error.kind() == ConfigErrorKind::Oversized => {}
            Err(error) => return Err(error),
        }
    }
    Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::AlreadyExists))
}

fn backup_name_with_stamp(relative: &str, stamp: u128) -> Result<String, ConfigError> {
    if relative.is_empty() || relative.contains(['/', '\\']) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    let backup = format!("{relative}.broken-{stamp}");
    if backup.len() > 200 || backup.ends_with(['.', ' ']) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    Ok(backup)
}

/// Reads one owned document through `decode`. A damaged document is
/// quarantined and salvaged from its stored bytes; a file the strict checks
/// reject is moved aside, and — for a permissions mismatch — salvaged from
/// the moved bytes before the `fallback` fills the rest. The published value
/// is the one returned.
pub(crate) fn load_or_reset<T>(
    home: &HomeLayout,
    relative: &'static str,
    max_bytes: usize,
    decode: impl FnOnce(&[u8]) -> Result<T, ConfigError>,
    fallback: impl FnOnce() -> T,
    salvage: impl FnOnce(&serde_json::Value) -> Option<T>,
    encode: impl FnOnce(&T) -> Result<Vec<u8>, ConfigError>,
) -> Result<Loaded<T>, ConfigError> {
    let bytes = match read_owned_file(home, relative, max_bytes) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ConfigErrorKind::Oversized => {
            read_owned_file(home, relative, QUARANTINE_MAX_BYTES.max(max_bytes))?
        }
        Err(error) if is_unusable_file(&error) => {
            return reset_unreadable(home, relative, error, max_bytes, fallback, salvage, encode);
        }
        Err(error) => return Err(error),
    };
    let Some(bytes) = bytes else {
        return Ok(Loaded {
            value: fallback(),
            repair: None,
        });
    };
    // The larger read may observe a newer, healthy save. Decode that snapshot
    // normally rather than resetting it because the earlier file was large.
    let decoded = if bytes.len() > max_bytes {
        Err(ConfigError::new(ConfigErrorKind::Oversized))
    } else {
        decode(bytes.as_slice())
    };
    match decoded {
        Ok(value) => Ok(Loaded {
            value,
            repair: None,
        }),
        Err(error) if is_document_damage(&error) => reset_from_bytes(
            home,
            relative,
            max_bytes,
            bytes.as_slice(),
            fallback,
            salvage,
            encode,
        ),
        Err(error) => Err(error),
    }
}

/// Moves a document the strict read rejected aside, unread, and publishes
/// the fallback — salvaging from the moved bytes when the failure was a
/// permissions mismatch. When nothing can be moved (the failure was the
/// home, not the file) the original error is returned unchanged.
///
/// An external writer such as magpie's agent plugin replaces the file
/// wholesale. Safe current-owner Windows ACL drift is handled before this
/// path. Any rejected bytes moved here must still pass owner, single-link,
/// and foreign-writer checks before salvage; moving an untrusted document
/// aside cannot make its contents trustworthy.
fn reset_unreadable<T>(
    home: &HomeLayout,
    relative: &'static str,
    error: ConfigError,
    max_bytes: usize,
    fallback: impl FnOnce() -> T,
    salvage: impl FnOnce(&serde_json::Value) -> Option<T>,
    encode: impl FnOnce(&T) -> Result<Vec<u8>, ConfigError>,
) -> Result<Loaded<T>, ConfigError> {
    let mut moved_backup = None;
    let stamp = crate::core::unix_now().as_nanos();
    for attempt in 0..16u128 {
        let candidate = backup_name_with_stamp(relative, stamp.saturating_add(attempt))?;
        match quarantine_rejected_owned_file(home, relative, &candidate, &error) {
            Ok(true) => {
                moved_backup = Some(candidate);
                break;
            }
            Ok(false) => return Err(error),
            Err(quarantine_err)
                if quarantine_err.io_kind() == Some(io::ErrorKind::AlreadyExists) => {}
            Err(quarantine_err) if quarantine_err.kind() == ConfigErrorKind::RevisionConflict => {
                return Err(quarantine_err);
            }
            Err(_) => return Err(error),
        }
    }
    let Some(backup) = moved_backup else {
        return Err(error);
    };
    let value = if error.kind() == ConfigErrorKind::AccessControl {
        read_quarantined_backup(home, &backup, max_bytes.max(QUARANTINE_MAX_BYTES))
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|document| salvage(&document))
            .unwrap_or_else(fallback)
    } else {
        fallback()
    };
    publish_repair(home, relative, max_bytes, None, encode(&value)?)?;
    Ok(Loaded {
        value,
        repair: Some(DocumentRepair {
            path: relative,
            backup,
        }),
    })
}

fn reset_from_bytes<T>(
    home: &HomeLayout,
    relative: &'static str,
    max_bytes: usize,
    bytes: &[u8],
    fallback: impl FnOnce() -> T,
    salvage: impl FnOnce(&serde_json::Value) -> Option<T>,
    encode: impl FnOnce(&T) -> Result<Vec<u8>, ConfigError>,
) -> Result<Loaded<T>, ConfigError> {
    let repair = quarantine_owned_bytes(home, relative, bytes)?;
    let value = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|document| salvage(&document))
        .unwrap_or_else(fallback);
    publish_repair(home, relative, max_bytes, Some(bytes), encode(&value)?)?;
    Ok(Loaded {
        value,
        repair: Some(repair),
    })
}

/// A repair only replaces the exact snapshot it salvaged. A newer save or
/// a recreated document after quarantine belongs to its writer, even when
/// the repair's older snapshot was invalid.
fn publish_repair(
    home: &HomeLayout,
    relative: &'static str,
    max_bytes: usize,
    expected: Option<&[u8]>,
    replacement: Vec<u8>,
) -> Result<(), ConfigError> {
    if replacement.len() > max_bytes {
        return Err(ConfigError::new(ConfigErrorKind::Oversized));
    }
    // Corrupt originals may exceed the document's normal read cap. The
    // replacement still obeys that cap; only comparison gets the larger one.
    locked_update_owned_file(
        home,
        relative,
        max_bytes.max(QUARANTINE_MAX_BYTES),
        |current| {
            if current != expected {
                return Err(ConfigError::new(ConfigErrorKind::RevisionConflict));
            }
            Ok(replacement)
        },
    )
}

/// Decodes `object[name]` as `T`, tolerating a missing member or a wrong
/// type. The document families' salvage paths use it so one bad member drops
/// only itself instead of resetting the whole file.
pub(crate) fn salvage_field<T: serde::de::DeserializeOwned>(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Option<T> {
    object
        .get(name)
        .and_then(|value| T::deserialize(value).ok())
}

/// Decodes every element of `object[name]` that is a valid `T`. A list with
/// one corrupt entry keeps its readable rows.
pub(crate) fn salvage_list<T: serde::de::DeserializeOwned>(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Vec<T> {
    object
        .get(name)
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| T::deserialize(row).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Applies `apply` on a clone of `state` and adopts the clone only when the
/// validation passes. Salvage walks keep every member that checks out
/// instead of resetting the whole document over one bad value.
pub(crate) fn salvage_adopt<D: Clone>(
    state: &mut D,
    ok: impl Fn(&D) -> bool,
    apply: impl FnOnce(&mut D),
) {
    let mut probe = state.clone();
    apply(&mut probe);
    if ok(&probe) {
        *state = probe;
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{
        HomeLayout, read_app_settings_with_repair, read_provider_secrets_with_repair,
        read_ui_state_with_repair, replace_app_settings,
    };

    fn home(label: &str) -> (tempfile_guard::Guard, HomeLayout) {
        let parent = std::env::temp_dir().join(format!(
            "mycode-repair-{label}-{}-{}",
            std::process::id(),
            crate::core::unix_now().as_nanos()
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

    /// Writes fixture bytes through the secure transaction with the exact
    /// private permissions required by the read path. Plain external writes
    /// may need Windows DACL normalization before their first read.
    fn write_private(home: &crate::config::HomeLayout, name: &str, bytes: &[u8]) {
        crate::config::write_owned_test_bytes(home, name, bytes)
            .unwrap_or_else(|error| panic!("fixture {name}: {error:?}"));
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
        write_private(&home, "settings.json", b"{not-json");
        write_private(&home, "ui.json", b"{\"formatVersion\":");
        write_private(&home, "secrets.json", b"\xff\xfe broken");

        let (settings, settings_repair) = read_app_settings_with_repair(&home).unwrap();
        assert_eq!(settings.appearance.font_size, "m");
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
    fn invalid_appearance_resets_settings_and_salvages_the_rest() {
        let (_guard, home) = home("appearance");
        let bytes = br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 3,
            "appearance": { "theme": "nope", "language": "zh" },
            "magpie": {
                "enabled": true,
                "baseUrl": "http://127.0.0.1:3425/v1",
                "apiKey": "magpie-mycode",
                "model": "deepseek/deepseek-chat"
            }
        }"#;
        write_private(&home, "settings.json", bytes);
        let (settings, repair) = read_app_settings_with_repair(&home).unwrap();
        // The broken theme fell back to the default; the healthy magpie
        // binding survived the reset.
        assert_eq!(settings.appearance.theme, "dark");
        assert_eq!(settings.appearance.language, "zh");
        assert!(settings.magpie.enabled);
        assert_eq!(settings.magpie.model, "deepseek/deepseek-chat");
        let repair = repair.expect("validation repair");
        let backup = std::fs::read(home.root().join(&repair.backup)).unwrap();
        assert!(
            backup.windows(4).any(|window| window == b"nope"),
            "backup lost"
        );
        assert!(backup.windows(6).any(|window| window == b"magpie"));
        let rewritten = std::fs::read(home.root().join("settings.json")).unwrap();
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.contains("\"magpie\""), "salvage published: {text}");
        assert!(!text.contains("nope"), "{text}");
    }

    #[test]
    fn valid_settings_load_clean_and_a_malformed_document_is_quarantined() {
        let (_guard, home) = home("valid");
        let mut settings = crate::config::AppSettings::default();
        settings.appearance.font_size = "l".to_owned();
        replace_app_settings(&home, crate::config::AuthorityRevision::ABSENT, &settings).unwrap();
        let (loaded, repair) = read_app_settings_with_repair(&home).unwrap();
        assert!(repair.is_none());
        assert_eq!(loaded.appearance.font_size, "l");

        // A retired field is document damage: the file is quarantined and
        // republished, with the still-valid `autoUpdate` kept.
        write_private(
            &home,
            "ui.json",
            br#"{"formatVersion":1,"kind":"mycode-ui-state","autoUpdate":false,"trustedProjects":["/legacy/root"]}"#,
        );
        let (ui, repair) = read_ui_state_with_repair(&home).unwrap();
        assert!(!ui.auto_update, "the valid field survived the reset");
        let repair = repair.expect("a rejected document is quarantined");
        let backup = std::fs::read(home.root().join(&repair.backup)).unwrap();
        assert!(backup.windows(7).any(|window| window == b"/legacy"));
        // The republished file parses, so the next launch is quiet.
        assert!(read_ui_state_with_repair(&home).unwrap().1.is_none());
    }
}
