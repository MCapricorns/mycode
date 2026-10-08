//! Value-redacted errors returned by configuration operations.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::error::Error as StdError;
use std::fmt::{self, Debug, Display, Formatter};
use std::io;
use std::path::{Path, PathBuf};

/// Classifies a configuration failure without exposing configuration values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigErrorKind {
    /// An owned home path is missing, relative, or lexically unsafe.
    InvalidHome,
    /// A requested owned path could escape its frozen hierarchy.
    PathEscape,
    /// A symlink or reparse point could redirect an owned path.
    LinkEscape,
    /// Native ownership or access control could not be enforced exactly.
    AccessControl,
    /// A source file could not be opened, read, or inspected.
    Io,
    /// JSON input was not valid UTF-8.
    NonUtf8,
    /// A byte bound was exceeded.
    Oversized,
    /// Input was not strict JSON, was partial, or had trailing content.
    InvalidJson,
    /// An owned authority document or value failed strict validation.
    AuthorityValidation,
    /// An authority document revision did not match the expected revision.
    RevisionConflict,
    /// An authority document revision cannot advance further.
    RevisionExhausted,
    /// A validated value could not be serialized.
    Serialization,
    /// The advisory write lock could not be acquired.
    Lock,
    /// A temporary file could not replace the destination.
    AtomicReplace,
}

struct ConfigErrorInner {
    kind: ConfigErrorKind,
    path: Option<PathBuf>,
    io_kind: Option<io::ErrorKind>,
    backtrace: Backtrace,
    detail: Option<String>,
}

/// Describes a configuration failure without retaining offending values.
///
/// Parse messages and input snippets are intentionally discarded. Callers can
/// branch on [`Self::kind`], inspect the path, and render the bounded summary
/// from [`Display`].
pub struct ConfigError {
    // Keeping contextual fields behind one allocation makes Result's error path
    // small while retaining a captured backtrace and native path information.
    inner: Box<ConfigErrorInner>,
}

impl ConfigError {
    pub(crate) fn new(kind: ConfigErrorKind) -> Self {
        Self {
            inner: Box::new(ConfigErrorInner {
                kind,
                path: None,
                io_kind: None,
                backtrace: Backtrace::capture(),
                detail: None,
            }),
        }
    }

    /// Attaches a field-path hint. Details must name fields and rules only —
    /// never values, which may carry secrets pasted into the wrong field.
    #[must_use]
    pub(crate) fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.inner.detail = Some(detail.into());
        self
    }

    /// Builds one authority-validation rejection for a Host-side storage
    /// callback (for example a compare-and-swap that lost or a document that
    /// failed strict revalidation).
    ///
    /// Host-substrate stores reuse the hardened owned-file transaction and
    /// signal domain-level rejections through this constructor; the stored
    /// document is never modified when it is returned.
    #[must_use]
    pub fn authority_rejection() -> Self {
        Self::new(ConfigErrorKind::AuthorityValidation)
    }

    pub(crate) fn for_path(kind: ConfigErrorKind, path: &Path) -> Self {
        let mut error = Self::new(kind);
        error.inner.path = Some(path.to_path_buf());
        error
    }

    pub(crate) fn with_io_kind(mut self, kind: io::ErrorKind) -> Self {
        self.inner.io_kind = Some(kind);
        self
    }

    /// Returns this failure's stable category.
    #[must_use]
    pub fn kind(&self) -> ConfigErrorKind {
        self.inner.kind
    }

    /// Returns the affected path, if the operation had one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    /// Returns the operating-system error category, if one was recorded.
    #[must_use]
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        self.inner.io_kind
    }

    /// Returns the backtrace captured where the error was classified.
    pub fn backtrace(&self) -> &Backtrace {
        &self.inner.backtrace
    }

    /// Renders the bounded summary without a captured backtrace.
    ///
    /// UI banners use this so a syntax rejection stays one line. The detail,
    /// when present, names a field or a line and column, never a value.
    #[must_use]
    pub fn summary(&self) -> String {
        let summary = match self.inner.kind {
            ConfigErrorKind::InvalidHome => "MYCode home path is invalid",
            ConfigErrorKind::PathEscape => "owned path component is invalid",
            ConfigErrorKind::LinkEscape => "owned path link traversal was rejected",
            ConfigErrorKind::AccessControl => "owned path access control failed",
            ConfigErrorKind::Io => "configuration file I/O failed",
            ConfigErrorKind::NonUtf8 => "configuration JSON is not UTF-8",
            ConfigErrorKind::Oversized => "configuration size limit was exceeded",
            ConfigErrorKind::InvalidJson => "configuration is not strict complete JSON",
            ConfigErrorKind::AuthorityValidation => "owned authority document is invalid",
            ConfigErrorKind::RevisionConflict => "owned authority revision conflict",
            ConfigErrorKind::RevisionExhausted => "owned authority revision is exhausted",
            ConfigErrorKind::Serialization => "configuration serialization failed",
            ConfigErrorKind::Lock => "configuration advisory lock failed",
            ConfigErrorKind::AtomicReplace => "configuration file replacement failed",
        };
        match &self.inner.detail {
            Some(detail) => format!("{summary}: {detail}"),
            None => summary.to_owned(),
        }
    }
}

impl Debug for ConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigError")
            .field("kind", &self.inner.kind)
            .field("path", &self.inner.path)
            .field("io_kind", &self.inner.io_kind)
            .field("backtrace", &self.inner.backtrace)
            .finish()
    }
}

impl Display for ConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.summary())?;
        if self.inner.backtrace.status() == BacktraceStatus::Captured {
            write!(formatter, "\n{}", self.inner.backtrace)?;
        }
        Ok(())
    }
}

impl StdError for ConfigError {}
