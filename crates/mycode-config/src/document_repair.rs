//! Backs up an unreadable owned document and republishes it canonical.
//!
//! Startup reads of `settings.json`, `secrets.json`, and `ui.json` use this
//! so a broken file cannot blank the window: damaged content, and a file the
//! strict checks reject for its own permissions, owner, or type, are both
//! moved aside and replaced. The replacement keeps every field that still
//! decodes and validates (`salvage`), so one bad section does not force the
//! user to re-enter the rest. The previous file stays beside the original as
//! `{name}.broken-<nanos>`. Session ledgers are not documents this module
//! handles.

use std::io;

use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::{
    locked_update_owned_file, quarantine_owned_file, read_owned_file,
};
use crate::{ConfigError, HomeLayout};

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
        ConfigErrorKind::AuthorityValidation
            | ConfigErrorKind::NonUtf8
            | ConfigErrorKind::Oversized
    )
}

/// Whether a failed read points at the document file itself — permissions,
/// owner, a link, or the wrong file type — rather than a passing I/O hiccup.
///
/// A file copied from another machine, or one whose ACL was stripped,
/// failed here with `owned path access control failed` on every launch.
fn is_unusable_file(error: &ConfigError) -> bool {
    match error.kind() {
        ConfigErrorKind::AccessControl | ConfigErrorKind::LinkEscape => true,
        ConfigErrorKind::Io => !matches!(
            error.io_kind(),
            Some(
                io::ErrorKind::Interrupted
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::ResourceBusy
            )
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
    let backup = publish_unique_backup(relative, crate::unix_now().as_nanos(), |backup| {
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

/// Reads one owned document through `decode`. A damaged or unusable file is
/// quarantined and replaced: `salvage` extracts whatever still validates from
/// the stored bytes, and the remainder comes from `fallback`. The published
/// value is the one returned.
pub(crate) fn load_or_reset<T>(
    home: &HomeLayout,
    relative: &'static str,
    max_bytes: usize,
    decode: impl FnOnce(&[u8]) -> Result<T, ConfigError>,
    fallback: impl FnOnce() -> T,
    salvage: impl FnOnce(&serde_json::Value) -> Option<T>,
    publish: impl FnOnce(&T) -> Result<(), ConfigError>,
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
                reset_from_bytes(home, relative, bytes.as_slice(), fallback, salvage, publish)
            }
            Err(error) => Err(error),
        },
        Err(error) if error.kind() == ConfigErrorKind::Oversized => {
            match read_owned_file(home, relative, QUARANTINE_MAX_BYTES.max(max_bytes)) {
                Ok(Some(bytes)) => {
                    reset_from_bytes(home, relative, bytes.as_slice(), fallback, salvage, publish)
                }
                Ok(None) => Ok(Loaded {
                    value: fallback(),
                    repair: None,
                }),
                Err(error) => Err(error),
            }
        }
        Err(error) if is_unusable_file(&error) => {
            reset_unreadable(home, relative, error, fallback, publish)
        }
        Err(error) => Err(error),
    }
}

/// Moves a document the strict read rejected aside, unread, and publishes
/// the fallback. When nothing can be moved (the failure was the home, not the
/// file) the original error is returned unchanged.
fn reset_unreadable<T>(
    home: &HomeLayout,
    relative: &'static str,
    error: ConfigError,
    fallback: impl FnOnce() -> T,
    publish: impl FnOnce(&T) -> Result<(), ConfigError>,
) -> Result<Loaded<T>, ConfigError> {
    let mut moved_backup = None;
    let stamp = crate::unix_now().as_nanos();
    for attempt in 0..16u128 {
        let candidate = backup_name_with_stamp(relative, stamp.saturating_add(attempt))?;
        match quarantine_owned_file(home, relative, &candidate) {
            Ok(true) => {
                moved_backup = Some(candidate);
                break;
            }
            Ok(false) => return Err(error),
            Err(quarantine_err)
                if quarantine_err.io_kind() == Some(io::ErrorKind::AlreadyExists) => {}
            Err(_) => return Err(error),
        }
    }
    let Some(backup) = moved_backup else {
        return Err(error);
    };
    let value = fallback();
    publish(&value)?;
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
    bytes: &[u8],
    fallback: impl FnOnce() -> T,
    salvage: impl FnOnce(&serde_json::Value) -> Option<T>,
    publish: impl FnOnce(&T) -> Result<(), ConfigError>,
) -> Result<Loaded<T>, ConfigError> {
    let repair = quarantine_owned_bytes(home, relative, bytes)?;
    let value = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|document| salvage(&document))
        .unwrap_or_else(fallback);
    publish(&value)?;
    Ok(Loaded {
        value,
        repair: Some(repair),
    })
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
    use super::quarantine_owned_bytes;
    use crate::{
        HomeLayout, read_app_settings_with_repair, read_provider_secrets_with_repair,
        read_ui_state_with_repair, replace_app_settings,
    };

    fn home(label: &str) -> (tempfile_guard::Guard, HomeLayout) {
        let parent = std::env::temp_dir().join(format!(
            "mycode-repair-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
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

    /// Writes fixture bytes through the app's secure transaction so the file
    /// carries the private permissions the read path enforces (a plain
    /// `fs::write` file inherits the temp directory's DACL and is rejected
    /// by the Windows fail-closed checks).
    fn write_private(home: &crate::HomeLayout, name: &str, bytes: &[u8]) {
        crate::write_owned_test_bytes(home, name, bytes)
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
            "providers": [{
                "id": "openai",
                "kind": "openai-completions",
                "baseUrl": "https://api.openai.com/v1",
                "models": ["gpt-4o"],
                "enabled": true
            }]
        }"#;
        write_private(&home, "settings.json", bytes);
        let (settings, repair) = read_app_settings_with_repair(&home).unwrap();
        // The broken theme fell back to the default; the healthy provider
        // row survived the reset.
        assert_eq!(settings.appearance.theme, "dark");
        assert_eq!(settings.appearance.language, "zh");
        assert_eq!(settings.providers.len(), 1);
        assert_eq!(settings.providers[0].id, "openai");
        let repair = repair.expect("validation repair");
        let backup = std::fs::read(home.root().join(&repair.backup)).unwrap();
        assert!(
            backup.windows(4).any(|window| window == b"nope"),
            "backup lost"
        );
        assert!(backup.windows(6).any(|window| window == b"openai"));
        let rewritten = std::fs::read(home.root().join("settings.json")).unwrap();
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.contains("\"openai\""), "salvage published: {text}");
        assert!(!text.contains("nope"), "{text}");
    }

    #[test]
    fn valid_settings_load_clean_and_a_malformed_document_is_quarantined() {
        let (_guard, home) = home("valid");
        let mut settings = crate::AppSettings::default();
        settings.appearance.font_size = "l".to_owned();
        replace_app_settings(&home, crate::AuthorityRevision::ABSENT, &settings).unwrap();
        let (loaded, repair) = read_app_settings_with_repair(&home).unwrap();
        assert!(repair.is_none());
        assert_eq!(loaded.appearance.font_size, "l");

        // A retired field is document damage: the file is quarantined and
        // republished, with the still-valid `autoUpdate` kept.
        write_private(
            &home,
            "ui.json",
            br#"{"formatVersion":1,"kind":"mycode-ui-state","autoUpdate":false,"workspaceRoots":["/legacy/root"]}"#,
        );
        let (ui, repair) = read_ui_state_with_repair(&home).unwrap();
        assert!(!ui.auto_update, "the valid field survived the reset");
        let repair = repair.expect("a rejected document is quarantined");
        let backup = std::fs::read(home.root().join(&repair.backup)).unwrap();
        assert!(backup.windows(7).any(|window| window == b"/legacy"));
        // The republished file parses, so the next launch is quiet.
        assert!(read_ui_state_with_repair(&home).unwrap().1.is_none());
    }

    fn names_in(home: &HomeLayout) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(home.root())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_file_the_strict_checks_reject_is_reset_instead_of_blocking_startup() {
        let (_guard, home) = home("unusable");
        // A plain write carries the wrong permissions for an owned document
        // (an inherited ACL on Windows, 0644 on Unix); read-only is what the
        // broken secrets.json in the field also carried.
        for name in ["settings.json", "secrets.json", "ui.json"] {
            let path = home.root().join(name);
            std::fs::write(&path, b"{\"copied\":\"from another machine\"}").unwrap();
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&path, permissions).unwrap();
        }

        let (settings, settings_repair) = read_app_settings_with_repair(&home).unwrap();
        assert_eq!(settings.appearance.theme, "dark");
        let (_, secrets_repair) = read_provider_secrets_with_repair(&home).unwrap();
        let (_, ui_repair) = read_ui_state_with_repair(&home).unwrap();
        for repair in [settings_repair, secrets_repair, ui_repair] {
            let repair = repair.expect("an unusable document is reported as reset");
            assert!(home.root().join(&repair.backup).exists(), "backup kept");
        }
        // The fresh documents read back cleanly, so the next launch is quiet.
        assert!(read_app_settings_with_repair(&home).unwrap().1.is_none());
        assert!(
            read_provider_secrets_with_repair(&home)
                .unwrap()
                .1
                .is_none()
        );
        assert!(read_ui_state_with_repair(&home).unwrap().1.is_none());
        for path in std::fs::read_dir(home.root()).unwrap().flatten() {
            let mut permissions = path.metadata().unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            let _ = std::fs::set_permissions(path.path(), permissions);
        }
    }

    #[test]
    fn writes_leave_no_lock_files_and_clear_old_ones() {
        let (_guard, home) = home("locks");
        // A home written by an older build still has its lock files.
        std::fs::write(home.root().join("settings.json.lock"), b"").unwrap();
        let mut settings = crate::AppSettings::default();
        settings.appearance.font_size = "l".to_owned();
        replace_app_settings(&home, crate::AuthorityRevision::ABSENT, &settings).unwrap();
        crate::replace_ui_state(&home, &crate::UiState::default()).unwrap();
        let names = names_in(&home);
        assert!(
            names.iter().all(|name| !name.ends_with(".lock")),
            "lock files left beside the documents: {names:?}"
        );
        assert!(names.contains(&"settings.json".to_owned()), "{names:?}");
    }

    #[test]
    fn a_decode_error_that_is_not_damage_is_reported_not_reset() {
        let (_guard, home) = home("rewrite");
        write_private(&home, "ui.json", b"{}");
        let error = super::load_or_reset(
            &home,
            "ui.json",
            1024,
            |_| -> Result<(), crate::ConfigError> {
                Err(crate::ConfigError::new(crate::ConfigErrorKind::Io))
            },
            || (),
            |_| -> Option<()> { None },
            |_| panic!("a read failure must not publish a replacement"),
        )
        .err()
        .expect("the decode failure surfaces");
        assert_eq!(error.kind(), crate::ConfigErrorKind::Io);
        assert!(
            names_in(&home)
                .iter()
                .all(|name| !name.contains(".broken-")),
            "a valid document was quarantined"
        );
    }

    #[test]
    fn quarantine_rejects_a_nested_path() {
        let (_guard, home) = home("path");
        let error = quarantine_owned_bytes(&home, "sessions/note.json", b"{}").unwrap_err();
        assert_eq!(error.kind(), crate::ConfigErrorKind::PathEscape);
    }

    #[test]
    fn backup_collision_retries_without_overwriting_the_existing_entry() {
        let (_guard, home) = home("fixed-collision");
        std::fs::create_dir_all(home.root()).unwrap();
        let source = home.root().join("settings.json");
        let backup = home.root().join("settings.json.broken-1");
        std::fs::write(&source, b"new damage").unwrap();
        std::fs::write(&backup, b"previous damage").unwrap();
        let error = super::quarantine_owned_file(&home, "settings.json", "settings.json.broken-1")
            .unwrap_err();
        assert_eq!(error.io_kind(), Some(std::io::ErrorKind::AlreadyExists));
        assert_eq!(std::fs::read(&source).unwrap(), b"new damage");
        assert_eq!(std::fs::read(&backup).unwrap(), b"previous damage");
        let mut names = Vec::new();
        let next = super::publish_unique_backup("settings.json", 1, |name| {
            names.push(name.to_owned());
            if names.len() == 1 {
                Err(crate::ConfigError::new(crate::ConfigErrorKind::Io)
                    .with_io_kind(std::io::ErrorKind::AlreadyExists))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(next, "settings.json.broken-2");
        assert_eq!(names, ["settings.json.broken-1", "settings.json.broken-2"]);
    }

    #[test]
    fn repeated_quarantines_do_not_overwrite_earlier_backups() {
        let (_guard, home) = home("collide");
        let first = quarantine_owned_bytes(&home, "settings.json", b"first-damage").unwrap();
        let second = quarantine_owned_bytes(&home, "settings.json", b"second-damage").unwrap();
        assert_ne!(first.backup, second.backup);
        assert_eq!(
            std::fs::read(home.root().join(&first.backup)).unwrap(),
            b"first-damage"
        );
        assert_eq!(
            std::fs::read(home.root().join(&second.backup)).unwrap(),
            b"second-damage"
        );
    }
}
