//! Immutable cwd, environment, and argv snapshot for the `shell` program launch.
//!
//! One preparation captures cwd, the sorted allowlisted environment (including
//! reconstructed PATH), argv, and the pinned executable. Every platform spawn
//! consumes that snapshot; none of them read the process environment.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use super::ResolveError;
use super::env::{env_path, native_os_len, snapshot_child_environment, sort_env};
use super::image::ImageKind;
use super::image::Shebang;
use super::resolve::{FileIdentity, PinnedImage, pin_program_with_path};
use crate::tool::ToolError;

/// Domain separator for the versioned invocation digest.
const INVOCATION_DIGEST_DOMAIN: &[u8] = b"mycode-tools exec-invocation v2";
/// Digest schema version mixed after the domain string.
const INVOCATION_DIGEST_VERSION: u64 = 2;

/// Immutable cwd, environment, argv, and pinned executable used for one spawn.
#[derive(Debug)]
pub(crate) struct PreparedInvocation {
    pinned: PinnedImage,
    argv0: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
    invocation_digest: [u8; 32],
    /// Script path when `program` was a direct shebang wrapper.
    wrapper: Option<String>,
}

impl PreparedInvocation {
    /// Snapshots cwd and allowlisted environment, pins `program`, and digests.
    ///
    /// Request and environment aggregate budgets are enforced before the
    /// snapshot, argv clone, and digest buffers are retained.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArgs`] when the request, environment, or
    /// image is rejected, and [`ToolError::Execution`] when cancelled.
    pub(super) fn prepare(
        session_cwd: &Path,
        program: &str,
        args: &[String],
        cancel: &CancellationToken,
    ) -> Result<Self, ToolError> {
        let env = snapshot_child_environment()?;
        Self::from_snapshot(session_cwd, program, args, &env, cancel)
            .map_err(ResolveError::into_tool_error)
    }

    /// Pins `program` with its canonical path as `argv[0]`.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::NotFound`] when PATH or path lookup misses the
    /// executable, and [`ResolveError::Other`] for every fail-closed rejection.
    pub(super) fn from_snapshot(
        session_cwd: &Path,
        program: &str,
        args: &[String],
        env: &[(OsString, OsString)],
        cancel: &CancellationToken,
    ) -> Result<Self, ResolveError> {
        Self::from_snapshot_inner(session_cwd, program, None, args, env, cancel)
    }

    /// Pins `program` while preserving `argv0` for alias-sensitive programs.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::NotFound`] when PATH or path lookup misses the
    /// executable, and [`ResolveError::Other`] for every fail-closed rejection.
    pub(super) fn from_snapshot_with_argv0(
        session_cwd: &Path,
        program: &str,
        argv0: &str,
        args: &[String],
        env: &[(OsString, OsString)],
        cancel: &CancellationToken,
    ) -> Result<Self, ResolveError> {
        Self::from_snapshot_inner(session_cwd, program, Some(argv0), args, env, cancel)
    }

    fn from_snapshot_inner(
        session_cwd: &Path,
        program: &str,
        argv0: Option<&str>,
        args: &[String],
        env: &[(OsString, OsString)],
        cancel: &CancellationToken,
    ) -> Result<Self, ResolveError> {
        let mut env = env.to_vec();
        sort_env(&mut env);
        let path_var = env_path(&env);
        let (pinned, launch_args, wrapper) =
            pin_or_follow_shebang(session_cwd, program, args, path_var, cancel)?;
        // A wrapper runs the interpreter. argv0 stays the interpreter image,
        // not the script name the caller typed.
        let argv0 = if wrapper.is_some() { None } else { argv0 };
        let argv0 = argv0.map_or_else(
            || {
                pinned
                    .canonical_path
                    .to_str()
                    .expect("pin_program validated canonical path Unicode")
                    .to_owned()
            },
            str::to_owned,
        );
        let invocation_digest = invocation_digest(
            &pinned.canonical_path,
            pinned.identity,
            &pinned.digest,
            &argv0,
            &launch_args,
            session_cwd,
            &env,
        );
        Ok(Self {
            pinned,
            argv0,
            args: launch_args,
            cwd: session_cwd.to_path_buf(),
            env,
            invocation_digest,
            wrapper,
        })
    }

    /// Script path when this launch followed a shebang wrapper.
    #[must_use]
    pub(super) fn wrapper(&self) -> Option<&str> {
        self.wrapper.as_deref()
    }

    /// SHA-256 over the canonical path, file identity, image digest, `argv0`,
    /// arguments, cwd, and environment.
    #[must_use]
    pub(super) fn invocation_digest(&self) -> &[u8; 32] {
        &self.invocation_digest
    }

    /// SHA-256 digest of the pinned image bytes.
    #[must_use]
    pub(super) fn image_digest(&self) -> &[u8; 32] {
        &self.pinned.digest
    }

    /// Native identity of the pinned image.
    #[must_use]
    pub(super) fn image_identity(&self) -> FileIdentity {
        self.pinned.identity
    }

    /// Classified kernel-loadable image kind.
    #[must_use]
    pub(super) fn image_kind(&self) -> ImageKind {
        self.pinned.kind
    }

    /// Canonical native path of the pinned executable.
    #[must_use]
    pub(super) fn canonical_path(&self) -> &Path {
        &self.pinned.canonical_path
    }

    /// Sorted allowlisted environment captured at preparation.
    #[must_use]
    pub(super) fn env(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    /// Splits the snapshot into its owned spawn inputs.
    pub(super) fn into_spawn_parts(
        self,
    ) -> (
        PinnedImage,
        String,
        Vec<String>,
        PathBuf,
        Vec<(OsString, OsString)>,
    ) {
        (self.pinned, self.argv0, self.args, self.cwd, self.env)
    }
}

fn pin_or_follow_shebang(
    session_cwd: &Path,
    program: &str,
    args: &[String],
    path_var: Option<&OsStr>,
    cancel: &CancellationToken,
) -> Result<(PinnedImage, Vec<String>, Option<String>), ResolveError> {
    match pin_program_with_path(session_cwd, program, args, path_var, cancel) {
        Ok(pinned) => Ok((pinned, args.to_vec(), None)),
        Err(ResolveError::Shebang { script, shebang }) => {
            follow_shebang(session_cwd, args, path_var, cancel, &script, &shebang)
        }
        Err(error) => Err(error),
    }
}

fn follow_shebang(
    session_cwd: &Path,
    args: &[String],
    path_var: Option<&OsStr>,
    cancel: &CancellationToken,
    script: &Path,
    shebang: &Shebang,
) -> Result<(PinnedImage, Vec<String>, Option<String>), ResolveError> {
    let (interpreter, mut launch_args) = shebang.launch_target();
    let script_arg = unicode_path(script)?;
    launch_args.push(script_arg.clone());
    launch_args.extend(args.iter().cloned());
    match pin_program_with_path(session_cwd, &interpreter, &launch_args, path_var, cancel) {
        Ok(pinned) => Ok((pinned, launch_args, Some(script_arg))),
        Err(ResolveError::Shebang { .. }) => {
            Err(ResolveError::Other(ToolError::InvalidArgs(format!(
                "{script_arg} is a wrapper script ({}). program mode followed it to \
                 `{interpreter}`, which is also a script. It follows only one shebang. \
                 Launch the final image yourself with program set to that image and the \
                 script path in args, or use mode script.",
                shebang.line
            ))))
        }
        Err(ResolveError::NotFound {
            program: missing,
            searched,
        }) => {
            let searched = searched
                .map(|count| format!(" ({count} PATH directories searched)"))
                .unwrap_or_default();
            Err(ResolveError::Other(ToolError::InvalidArgs(format!(
                "{script_arg} is a wrapper script ({}). program mode runs `{missing}` with \
                 the script path and your args, but that interpreter was not found{searched}. \
                 Use mode script, or pass the interpreter path as program.",
                shebang.line
            ))))
        }
        Err(error) => Err(error),
    }
}

fn unicode_path(path: &Path) -> Result<String, ResolveError> {
    path.to_str()
        .filter(|text| !text.contains('\0'))
        .map(str::to_owned)
        .ok_or_else(|| {
            ResolveError::Other(ToolError::InvalidArgs(
                "wrapper script path is not valid Unicode and cannot be passed as an argument"
                    .into(),
            ))
        })
}

/// Length-framed SHA-256 over the effective launch identity.
pub(super) fn invocation_digest(
    canonical_path: &Path,
    identity: FileIdentity,
    image_digest: &[u8; 32],
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    frame_bytes(&mut hasher, INVOCATION_DIGEST_DOMAIN);
    hasher.update(INVOCATION_DIGEST_VERSION.to_be_bytes());
    frame_os(&mut hasher, canonical_path.as_os_str());
    frame_identity(&mut hasher, identity);
    frame_bytes(&mut hasher, image_digest);
    frame_bytes(&mut hasher, argv0.as_bytes());
    hasher.update(u64::try_from(args.len()).unwrap_or(u64::MAX).to_be_bytes());
    for argument in args {
        frame_bytes(&mut hasher, argument.as_bytes());
    }
    frame_os(&mut hasher, cwd.as_os_str());
    hasher.update(u64::try_from(env.len()).unwrap_or(u64::MAX).to_be_bytes());
    for (key, value) in env {
        frame_os(&mut hasher, key);
        frame_os(&mut hasher, value);
    }
    hasher.finalize().into()
}

fn frame_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn frame_os(hasher: &mut Sha256, value: &OsStr) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        frame_bytes(hasher, value.as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let units: Vec<u16> = value.encode_wide().collect();
        let byte_len = u64::try_from(units.len().saturating_mul(2)).unwrap_or(u64::MAX);
        hasher.update(byte_len.to_be_bytes());
        for unit in units {
            hasher.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        frame_bytes(hasher, value.as_encoded_bytes());
    }
}

fn frame_identity(hasher: &mut Sha256, identity: FileIdentity) {
    #[cfg(unix)]
    {
        hasher.update(identity.device.to_be_bytes());
        hasher.update(identity.inode.to_be_bytes());
    }
    #[cfg(windows)]
    {
        hasher.update(identity.volume.to_be_bytes());
        hasher.update(identity.file_id);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = identity;
        hasher.update(0u64.to_be_bytes());
    }
}

/// Redacted environment lengths for UI details. Values are never copied here.
pub(super) fn environment_summary(env: &[(OsString, OsString)], limit: usize) -> serde_json::Value {
    let key_byte_lengths: Vec<usize> = env
        .iter()
        .take(limit)
        .map(|(key, _)| native_os_len(key))
        .collect();
    let value_byte_lengths: Vec<usize> = env
        .iter()
        .take(limit)
        .map(|(_, value)| native_os_len(value))
        .collect();
    serde_json::json!({
        "count": env.len(),
        "key_byte_lengths": key_byte_lengths,
        "value_byte_lengths": value_byte_lengths,
        "omitted": env.len().saturating_sub(limit),
    })
}
