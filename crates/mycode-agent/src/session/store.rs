//! SQLite session index and JSONL event logs.
//!
//! `<home>/sessions.db` (SQLite, WAL) is the commit authority for titles,
//! branch heads, event counts, and byte offsets.
//! `<home>/sessions/<project key>/<id>/<branch>.jsonl` is the event source of
//! truth: one JSON object per line. The project key is the folder the
//! session was created in ([`mycode_config::project_dir_key`]). Payloads at or
//! above [`EXTERNAL_PAYLOAD_THRESHOLD`] live in `payloads/<event-id>.bin`;
//! the line names the file and carries its digest. They are never stored as
//! SQLite blobs.
//!
//! Listing reads the index only. Opening a branch loads the offset rows, then
//! payload reads seek one line. An append writes one line, then updates the
//! index in one transaction under the session actor.
mod index;
mod jsonl;

#[cfg(test)]
mod tests;

pub(crate) use index::{
    AppendRequest, AppendedLocation, BranchCommitRequest, ListedSession, SessionStore, StoreError,
    StoredEvent, decode_head, delete_indexed_session, list_sessions, rename_indexed_session,
};
pub(crate) use jsonl::{charge, encoded_record_len};

/// SQLite index file name at the owned home root.
pub(crate) const SESSIONS_DB_FILE: &str = "sessions.db";
/// Maximum committed charge across one session: 512 MiB.
pub(crate) const MAX_SESSION_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Payloads at or above this size are written beside the JSONL log.
pub(crate) const EXTERNAL_PAYLOAD_THRESHOLD: usize = 64 * 1024;
/// JSONL file suffix.
pub(crate) const JSONL_SUFFIX: &str = ".jsonl";
/// Directory of externalized payloads inside one session.
pub(crate) const PAYLOADS_DIR: &str = "payloads";
/// External payload file suffix.
pub(crate) const PAYLOAD_FILE_SUFFIX: &str = ".bin";

use std::path::PathBuf;

use mycode_config::HomeLayout;

use super::ids::{BranchId, SessionEventId, SessionId};

/// Constructs validated owned paths below one session directory.
pub(crate) struct SessionPaths<'a> {
    home: &'a HomeLayout,
    /// Relative owned path of the session directory.
    dir: String,
}

impl<'a> SessionPaths<'a> {
    /// Binds one session directory, given as a relative owned path.
    pub(crate) const fn at(home: &'a HomeLayout, dir: String) -> Self {
        Self { home, dir }
    }

    /// Binds the directory `session` already has. A session whose directory
    /// is gone gets a fresh one under [`mycode_config::NO_PROJECT_DIR`], so
    /// an empty session can still take its first write.
    pub(crate) fn locate(home: &'a HomeLayout, session: &SessionId) -> Result<Self, StoreError> {
        let dir = match mycode_config::locate_session_dir(home, session.as_str()) {
            Ok(Some(dir)) => dir,
            Ok(None) => mycode_config::new_session_dir(None, session.as_str())
                .map_err(|_| StoreError::Unavailable)?,
            Err(_) => return Err(StoreError::Storage),
        };
        Ok(Self::at(home, dir))
    }

    fn relative(&self, tail: &str) -> String {
        format!("{}{tail}", self.dir)
    }

    /// Relative owned path of the session directory.
    pub(crate) fn session_dir(&self) -> String {
        self.relative("")
    }

    /// Relative owned path of one branch JSONL log.
    pub(crate) fn branch_jsonl(&self, branch: &BranchId) -> String {
        self.relative(&format!("/{}{JSONL_SUFFIX}", branch.as_str()))
    }

    /// Relative owned path of the external payload directory.
    pub(crate) fn payloads_dir(&self) -> String {
        self.relative(&format!("/{PAYLOADS_DIR}"))
    }

    /// Relative owned path of one externalized payload.
    pub(crate) fn payload_file(&self, event: &SessionEventId) -> String {
        format!(
            "{}/{}{PAYLOAD_FILE_SUFFIX}",
            self.payloads_dir(),
            event.as_str()
        )
    }

    /// Absolute path for one relative owned path.
    ///
    /// # Errors
    ///
    /// Returns the mycode-config path-escape error when a component is unsafe.
    pub(crate) fn absolute(&self, relative: &str) -> Result<PathBuf, mycode_config::ConfigError> {
        self.home.owned_join(relative)
    }
}

/// Absolute path of the session index database.
pub(crate) fn database_path(home: &HomeLayout) -> Result<PathBuf, mycode_config::ConfigError> {
    home.owned_join(SESSIONS_DB_FILE)
}
