//! GitHub-release based self-update for the MYCode binaries.
//!
//! The checker resolves the latest published release, picks the release
//! asset matching the running platform and binary — published names are
//! `<exe stem>-v<version>-<target>.zip`, so `mycode-cli` and
//! `mycode-desktop` update from disjoint archives — and compares
//! semantic versions. The
//! installer downloads the asset plus its `.sha256` sidecar, verifies the
//! digest, extracts the new binary, and persists it as the durable pending
//! payload under `<home>/update/`. Staging arms the platform swap right
//! away: macOS hands it to a detached shell script that replaces the binary
//! in place, and Windows copies the running executable into a detached
//! helper that waits for this process to leave — exit by any route
//! (quit, crash, kill) lands the update, so the next launch is the new
//! version. A launch that still finds a live pending payload re-arms it,
//! which covers swaps lost to a reboot or a dead helper. The install
//! buttons arm the same swap with `--relaunch` (or the script equivalent)
//! and quit, so a click restarts straight into the new version. Asset
//! names and staging conventions are produced by the release jobs in
//! `.github/workflows/ci.yml`. Pull requests run only the check job, with
//! no packaging or publish. A push to main builds the archives and publishes
//! when the release plan says to.

pub(crate) mod apply;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;

use serde::Deserialize;
use sha2::Digest as _;

/// GitHub API endpoint resolving the latest published release.
pub const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/MCapricorns/mycode/releases/latest";

/// Every update download lives under this repository's release files. The
/// API answer names the URL, so it is checked before anything is fetched.
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/MCapricorns/mycode/releases/download/";

/// Maximum accepted update asset size: 512 MiB.
pub const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum accepted checksum sidecar size.
const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
/// Binary size guard for the extracted payload.
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;

/// The running application version (workspace version).
#[must_use]
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Release asset suffix for one target, empty when that target is unpublished.
///
/// Published archives are Windows x64, Windows ARM64, and macOS Apple
/// Silicon. Intel macOS, Windows x86, Linux, and every other target resolve
/// no asset and stay on their installed version.
fn asset_suffix_for(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("windows", "x86_64") => "-x86_64-pc-windows-msvc.zip",
        ("windows", "aarch64") => "-aarch64-pc-windows-msvc.zip",
        ("macos", "aarch64") => "-aarch64-apple-darwin.zip",
        _ => "",
    }
}

/// Published asset family of the running executable: `mycode-cli` for the
/// terminal binary, `mycode-desktop` otherwise.
///
/// Only two families are published, so the stem is mapped onto them rather
/// than used verbatim: a desktop binary the user renamed (or a lookup
/// failure) must keep updating from the desktop family instead of asking
/// the release for an asset that never exists. A pre-rename `mycode`
/// binary keeps updating into the CLI family — the installed copy lands as
/// `mycode-cli` beside it.
fn exe_stem() -> String {
    let stem = std::env::current_exe().ok().and_then(|path| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    });
    asset_family(stem.as_deref()).to_owned()
}

fn asset_family(stem: Option<&str>) -> &'static str {
    match stem {
        Some(stem)
            if stem.eq_ignore_ascii_case("mycode-cli") || stem.eq_ignore_ascii_case("mycode") =>
        {
            "mycode-cli"
        }
        _ => "mycode-desktop",
    }
}

/// Release-asset prefix for one executable stem: published names look
/// like `{stem}-v0.9.18-x86_64-pc-windows-msvc.zip`. The trailing `-v`
/// keeps the two families disjoint even though `mycode` is a strict
/// prefix of `mycode-desktop`.
fn asset_prefix(exe_stem: &str) -> String {
    format!("{exe_stem}-v")
}

/// One installable update offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateOffer {
    /// New version (no `v` prefix).
    pub version: String,
    /// Release page URL for humans.
    pub notes_url: String,
    /// Asset download URL.
    pub asset_url: String,
    /// Checksum sidecar URL.
    pub checksum_url: String,
}

/// A downloaded and verified update, staged for the swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedUpdate {
    /// New version (no `v` prefix), for "newer than me" checks and prompts.
    pub version: String,
    /// Staged new binary, ready to replace the running executable.
    pub new_binary: PathBuf,
    /// Directory holding the staged binary and helper files. A temporary
    /// download stage is swept at startup; a persisted payload lives under
    /// `<home>/update`.
    pub stage_dir: PathBuf,
    /// SHA-256 of `new_binary`. The swap refuses to run when the file differs.
    pub binary_sha256: String,
}

#[derive(Deserialize)]
struct ReleaseJson {
    #[serde(rename = "tagName", alias = "tag_name")]
    tag_name: String,
    #[serde(rename = "htmlUrl", alias = "html_url")]
    html_url: String,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    #[serde(rename = "browserDownloadUrl", alias = "browser_download_url")]
    browser_download_url: String,
}

/// Resolves the latest installable release for this target.
///
/// `Ok(None)` means there is nothing to install: the app is current, the
/// target has no published archive, GitHub returned 404, or the tag is
/// not a newer semver version. The version gate runs before the asset
/// lookup, so an up-to-date app stays quiet even when the current release
/// is missing its zip. A newer release whose zip is missing is `Err`, not
/// `None`, so a broken release job is visible instead of silently ignored.
///
/// # Errors
///
/// Returns the transport, parse, or HTTP failure message. An unparsable
/// tag is `Ok(None)`, not an error.
pub async fn latest_release(user_agent: &str) -> Result<Option<UpdateOffer>, String> {
    let response = mycode_providers::send_pinned(mycode_providers::PinnedRequest {
        method: reqwest::Method::GET,
        url: LATEST_RELEASE_API.to_owned(),
        headers: vec![(
            "accept".to_owned(),
            "application/vnd.github+json".to_owned(),
        )],
        body: None,
        mode: mycode_providers::PinMode::PublicHttps,
        timeout: Some(std::time::Duration::from_secs(30)),
        read_timeout: None,
        user_agent: Some(user_agent.to_owned()),
        follow_redirects: true,
        cancel: tokio_util::sync::CancellationToken::new(),
    })
    .await
    .map_err(|error| format!("update check failed: {}", brief_error(&error)))?;
    if classify_release_status(response.status())?.is_none() {
        return Ok(None);
    }
    let release: ReleaseJson = response.json().await.map_err(|error| {
        format!(
            "update response malformed: {}",
            brief_error(&error.to_string())
        )
    })?;
    if !is_newer(&release.tag_name, current_version()) {
        return Ok(None);
    }
    resolve_asset(&release.tag_name, &release.html_url, &release.assets)
}

/// First line of a transport error, bounded, so reqwest's full error chain
/// (URL, TLS, and retry diagnostics) cannot stretch a toast across the screen.
pub fn brief_error(message: &str) -> String {
    let first = message.lines().next().unwrap_or(message);
    let mut chars = first.chars();
    let mut brief = String::new();
    for char in chars.by_ref().take(160) {
        brief.push(char);
    }
    if chars.next().is_some() {
        brief.push('\u{2026}');
    }
    brief
}

/// Classifies one fetched release-check status: `Ok(None)` short-circuits as
/// "no release published" (404), `Ok(Some(()))` proceeds to parse the body,
/// and every other status is a surfaced failure. Reporting a server error,
/// auth failure, or rate limit as "current" would silently hide a broken
/// updater.
fn classify_release_status(status: reqwest::StatusCode) -> Result<Option<()>, String> {
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("update check failed: HTTP {status}"));
    }
    Ok(Some(()))
}

fn resolve_asset(
    tag: &str,
    notes_url: &str,
    assets: &[AssetJson],
) -> Result<Option<UpdateOffer>, String> {
    resolve_asset_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        &exe_stem(),
        tag,
        notes_url,
        assets,
    )
}

/// `Ok(None)` is an unsupported target (no published archive). A supported
/// target with no matching file is an error, so a supported platform is not
/// reported as already up to date when its zip is missing from the release.
fn resolve_asset_for(
    os: &str,
    arch: &str,
    exe_stem: &str,
    tag: &str,
    notes_url: &str,
    assets: &[AssetJson],
) -> Result<Option<UpdateOffer>, String> {
    let suffix = asset_suffix_for(os, arch);
    if suffix.is_empty() {
        return Ok(None);
    }
    // Published names look like
    // `mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip`. The `.sha256`
    // sidecar shares the prefix and must not be selected.
    let prefix = asset_prefix(exe_stem);
    let asset = assets.iter().find(|asset| {
        asset.name.starts_with(&prefix)
            && asset.name.ends_with(suffix)
            && !asset.name.ends_with(".sha256")
    });
    let Some(asset) = asset else {
        return Err(format!(
            "update check failed: release has no {prefix}*{suffix} asset"
        ));
    };
    if !is_release_download(&asset.browser_download_url) {
        return Err("update check failed: asset is not a MYCode release download".to_owned());
    }
    let version = tag.trim_start_matches('v').to_owned();
    Ok(Some(UpdateOffer {
        version,
        notes_url: notes_url.to_owned(),
        asset_url: asset.browser_download_url.clone(),
        checksum_url: format!("{}.sha256", asset.browser_download_url),
    }))
}

/// A plain file under [`RELEASE_DOWNLOAD_PREFIX`]: no query, fragment, or
/// dot segment that could point the download somewhere else.
fn is_release_download(url: &str) -> bool {
    url.strip_prefix(RELEASE_DOWNLOAD_PREFIX)
        .is_some_and(|rest| {
            !rest.is_empty()
                && !rest.contains(['?', '#', '\\', '%'])
                && rest
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
        })
}

/// Whether `tag` is strictly newer than `current` (both `vX.Y.Z` or `X.Y.Z`).
#[must_use]
pub fn is_newer(tag: &str, current: &str) -> bool {
    let (Ok(tag), Ok(current)) = (
        semver::Version::parse(tag.trim_start_matches('v')),
        semver::Version::parse(current.trim_start_matches('v')),
    ) else {
        return false;
    };
    tag > current
}

/// Downloads the update asset, verifies its published SHA-256 digest, and
/// extracts the new binary into a fresh staging directory.
///
/// # Errors
///
/// Returns a failure message; the running installation is untouched.
///
/// The bridge core runs on a single-threaded runtime, so every multi-hundred-
/// megabyte synchronous stage (checksum, zip extraction, hashing) runs on
/// the blocking pool; only the HTTP streams stay on the async thread.
pub async fn download_update(
    user_agent: &str,
    offer: &UpdateOffer,
) -> Result<PreparedUpdate, String> {
    let stage_dir = std::env::temp_dir().join(format!(
        "mycode-update-{}-{}",
        std::process::id(),
        offer.version.replace('.', "-")
    ));
    let stage_reset = stage_dir.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        if stage_reset.exists() {
            std::fs::remove_dir_all(&stage_reset)
                .map_err(|error| format!("stage reset: {error}"))?;
        }
        std::fs::create_dir_all(&stage_reset).map_err(|error| format!("stage create: {error}"))
    })
    .await
    .map_err(|error| format!("stage task: {error}"))??;

    let asset_path = stage_dir.join("update.asset");
    fetch_to_file(user_agent, &offer.asset_url, &asset_path, MAX_ASSET_BYTES).await?;

    let checksum_path = stage_dir.join("update.sha256");
    fetch_to_file(
        user_agent,
        &offer.checksum_url,
        &checksum_path,
        MAX_CHECKSUM_BYTES,
    )
    .await?;

    let verify_stage_dir = stage_dir.clone();
    let exe = exe_stem();
    let verify_stage = tokio::task::spawn_blocking(move || -> Result<(PathBuf, String), String> {
        verify_checksum(&asset_path, &checksum_path)?;
        let binary_path = extract_binary(&asset_path, &verify_stage_dir, &exe)?;
        let binary_sha256 = file_sha256(&binary_path)?;
        let _ = std::fs::remove_file(&asset_path);
        let _ = std::fs::remove_file(&checksum_path);
        Ok((binary_path, binary_sha256))
    });
    let (new_binary, binary_sha256) = verify_stage
        .await
        .map_err(|error| format!("verify task: {error}"))??;
    Ok(PreparedUpdate {
        version: offer.version.clone(),
        new_binary,
        stage_dir,
        binary_sha256,
    })
}

/// Idle ceiling while waiting for response headers and between body
/// chunks of an update download. The download itself is streaming: a
/// total request timeout would cap a multi-hundred-megabyte asset at one
/// fixed wall clock and fail every link slower than that.
const DOWNLOAD_IDLE: std::time::Duration = std::time::Duration::from_secs(30);

async fn fetch_to_file(
    user_agent: &str,
    url: &str,
    destination: &Path,
    maximum: u64,
) -> Result<(), String> {
    let mut response = mycode_providers::send_pinned(mycode_providers::PinnedRequest {
        method: reqwest::Method::GET,
        url: url.to_owned(),
        headers: Vec::new(),
        body: None,
        mode: mycode_providers::PinMode::PublicHttps,
        timeout: None,
        read_timeout: Some(DOWNLOAD_IDLE),
        user_agent: Some(user_agent.to_owned()),
        follow_redirects: true,
        cancel: tokio_util::sync::CancellationToken::new(),
    })
    .await
    .map_err(|error| format!("download failed: {}", brief_error(&error)))?;
    if !response.status().is_success() {
        return Err(format!("download returned {}", response.status()));
    }
    // Disk writes happen on a blocking-pool task fed by a bounded channel,
    // so a fast network never parks the async thread inside write(2) and a
    // slow disk applies backpressure instead of buffering the asset.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(32);
    let writer_destination = destination.to_path_buf();
    let writer = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::create(&writer_destination)
            .map_err(|error| format!("stage: {error}"))?;
        let mut writer = std::io::BufWriter::new(file);
        while let Some(chunk) = rx.blocking_recv() {
            writer
                .write_all(&chunk)
                .map_err(|error| format!("stage write failed: {error}"))?;
        }
        writer
            .flush()
            .map_err(|error| format!("stage write failed: {error}"))
    });
    let mut downloaded: u64 = 0;
    loop {
        // `chunk` is cancel-safe, so the idle ceiling only fires on a
        // connection that stopped delivering bytes.
        let chunk = match tokio::time::timeout(DOWNLOAD_IDLE, response.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => return Err(format!("download stream failed: {error}")),
            Err(_) => return Err("download stalled: no data arrived in time".to_owned()),
        };
        downloaded += chunk.len() as u64;
        if downloaded > maximum {
            return Err("download exceeded its size bound".to_owned());
        }
        if tx.send(chunk).await.is_err() {
            // The writer task already failed; its result carries the reason.
            break;
        }
    }
    drop(tx);
    writer
        .await
        .map_err(|error| format!("stage task: {error}"))?
}

fn verify_checksum(asset: &Path, checksum_file: &Path) -> Result<(), String> {
    let mut sidecar = String::new();
    std::fs::File::open(checksum_file)
        .and_then(|mut file| file.read_to_string(&mut sidecar))
        .map_err(|error| format!("checksum sidecar: {error}"))?;
    let expected = sidecar
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("checksum sidecar malformed".to_owned());
    }
    let actual = file_sha256(asset)?;
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err("downloaded update failed its checksum verification".to_owned());
    }
    Ok(())
}

fn extract_binary(asset: &Path, stage_dir: &Path, exe_stem: &str) -> Result<PathBuf, String> {
    let file = std::fs::File::open(asset).map_err(|error| format!("asset open: {error}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| format!("asset open: {error}"))?;
    let mut entry_name: Option<String> = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("asset entry: {error}"))?;
        let name = entry
            .name()
            .map_err(|error| format!("asset entry: {error}"))?
            .into_owned();
        if entry.is_dir() {
            continue;
        }
        let base = name.split('/').next_back().unwrap_or(&name);
        let executable = name.ends_with(".exe") || base == exe_stem;
        if !executable {
            continue;
        }
        let mut parts = std::path::Path::new(&name).components();
        // A drive prefix (`C:x.exe`) would make the staged path absolute on
        // Windows, so a colon is refused along with `..` and roots.
        let malicious = name.contains(['\\', ':'])
            || parts.any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
        if malicious {
            return Err("archive entry path is unsafe".to_owned());
        }
        if entry.size() > MAX_BINARY_BYTES {
            return Err("archive entry oversized".to_owned());
        }
        entry_name = Some(name);
        break;
    }
    let Some(entry_name) = entry_name else {
        return Err("archive has no application binary".to_owned());
    };
    let base_name = entry_name
        .split('/')
        .next_back()
        .unwrap_or(exe_stem)
        .to_owned();
    let binary_path = stage_dir.join(&base_name);
    let mut entry = archive
        .by_name(&entry_name)
        .map_err(|error| format!("asset entry: {error}"))?;
    let mut out =
        std::fs::File::create(&binary_path).map_err(|error| format!("stage binary: {error}"))?;
    // Chunked copy: a lying zip header could declare a small `size()` and
    // still decompress more bytes, so the guard is enforced on what is
    // actually written.
    let mut buffer = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let read = entry
            .read(&mut buffer)
            .map_err(|error| format!("stage binary: {error}"))?;
        if read == 0 {
            break;
        }
        written += read as u64;
        if written > MAX_BINARY_BYTES {
            return Err("archive entry oversized".to_owned());
        }
        out.write_all(&buffer[..read])
            .map_err(|error| format!("stage binary: {error}"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("stage binary: {error}"))?;
    }
    Ok(binary_path)
}

/// Arms the platform swap and returns. The caller may exit right away (an
/// install click) or keep running (a staged download): on Windows the
/// detached helper waits for this process to leave, on Unix the script
/// replaces the binary in place immediately — either way the next launch
/// is the new version, no matter how this process ends.
///
/// `relaunch` starts the target again after a verified replace. A terminal
/// binary is never relaunched: it cannot get its console back, so the
/// user's next `mycode-cli` run is simply the new version.
///
/// Windows launches a copy of this executable with [`apply::APPLY_UPDATE_ARG`].
/// macOS launches an `update-*.sh` script.
///
/// # Errors
///
/// Returns a failure message when the staged payload is corrupt and the
/// target does not already carry it, or when the helper cannot start.
pub fn arm_staged_update(prepared: &PreparedUpdate, relaunch: bool) -> Result<(), String> {
    // A terminal binary can never relaunch: the helper cannot hand its
    // console back, so the next `mycode-cli` run is the new version.
    let relaunch = relaunch && !is_cli_binary();
    let current = std::env::current_exe().map_err(|error| format!("current exe: {error}"))?;
    let current = current.canonicalize().unwrap_or(current);
    // A sibling helper may already have landed the swap, and a winner may
    // already have consumed the staged file, so a missing staged file is
    // fine whenever the target itself already hashes to the expected digest.
    let applied = target_matches(&current, &prepared.binary_sha256);
    if !applied {
        let staged = file_sha256(&prepared.new_binary)
            .map_err(|_| "staged update binary is unreadable".to_owned())?;
        if !staged.eq_ignore_ascii_case(&prepared.binary_sha256) {
            return Err("staged update binary failed its sha256 check".to_owned());
        }
    }
    #[cfg(windows)]
    {
        apply::spawn_windows_helper(
            &prepared.stage_dir,
            &current,
            &prepared.new_binary,
            &prepared.binary_sha256,
            relaunch,
        )?;
    }
    #[cfg(not(windows))]
    {
        let script_path = prepared.stage_dir.join(format!(
            "update-{}-{}.sh",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let script = unix_script(
            &prepared.new_binary,
            &current,
            &prepared.binary_sha256,
            relaunch,
        );
        std::fs::write(&script_path, script).map_err(|error| format!("updater script: {error}"))?;
        Command::new("/bin/sh")
            .arg(&script_path)
            .spawn()
            .map_err(|error| format!("updater spawn: {error}"))?;
    }
    Ok(())
}

/// Whether `target` already hashes to `expected` — the swap is done.
fn target_matches(target: &Path, expected: &str) -> bool {
    file_sha256(target).is_ok_and(|hash| hash.eq_ignore_ascii_case(expected))
}

/// Durable directory for a staged update that must survive this process:
/// `<home>/update/`. A payload here outlives crashes and reboots, so a
/// later launch can finish a swap no exit path ever ran.
pub fn pending_dir(home: &mycode_config::HomeLayout) -> PathBuf {
    home.root().join("update")
}

/// Filenames inside [`pending_dir`] for one binary family: the payload,
/// its expected digest, the pending version, and the startup-apply counter
/// that bounds relaunch loops.
fn pending_paths(dir: &Path, stem: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        dir.join(format!("{stem}.pending")),
        dir.join(format!("{stem}.sha256")),
        dir.join(format!("{stem}.version")),
        dir.join(format!("{stem}.attempts")),
    )
}

/// How many startups may arm one payload before it is dropped. A payload
/// whose swap keeps failing (a lock held by a surviving instance, say)
/// must not bounce every launch forever.
const MAX_PENDING_ATTEMPTS: u32 = 3;

/// Moves a verified stage into `<home>/update/` as this binary family's
/// pending payload: `{stem}.pending`, `{stem}.sha256`, `{stem}.version`.
/// The durable copy survives process exit, so a quit by any route — or
/// the next launch when the armed helper is gone — still lands the swap.
///
/// # Errors
///
/// Returns a failure message; the running installation is untouched.
pub fn persist_pending_update(
    home: &mycode_config::HomeLayout,
    prepared: &PreparedUpdate,
) -> Result<PreparedUpdate, String> {
    let dir = pending_dir(home);
    std::fs::create_dir_all(&dir).map_err(|error| format!("pending dir: {error}"))?;
    let (pending, sha_file, version_file, _) = pending_paths(&dir, &exe_stem());
    let staging = pending.with_extension("moving");
    std::fs::copy(&prepared.new_binary, &staging)
        .map_err(|error| format!("pending copy: {error}"))?;
    let actual = file_sha256(&staging)?;
    if !actual.eq_ignore_ascii_case(&prepared.binary_sha256) {
        let _ = std::fs::remove_file(&staging);
        return Err("pending copy failed its sha256 check".to_owned());
    }
    std::fs::rename(&staging, &pending).map_err(|error| format!("pending move: {error}"))?;
    std::fs::write(&sha_file, format!("{}\n", prepared.binary_sha256))
        .map_err(|error| format!("pending digest: {error}"))?;
    // The version file is written last: it is the marker reconcile needs,
    // so a crash mid-persist leaves an incomplete payload to clean up.
    std::fs::write(&version_file, format!("{}\n", prepared.version))
        .map_err(|error| format!("pending version: {error}"))?;
    // The temp stage is consumed; a reset at startup sweeps leftovers.
    let _ = std::fs::remove_dir_all(&prepared.stage_dir);
    Ok(PreparedUpdate {
        version: prepared.version.clone(),
        new_binary: pending,
        stage_dir: dir,
        binary_sha256: prepared.binary_sha256.clone(),
    })
}

/// Whether the running executable updates from the `mycode-cli` asset
/// family (vs `mycode-desktop`). The terminal binary is never relaunched
/// by the swap: a helper cannot hand a console back to it, so the next
/// `mycode-cli` run is the new version.
#[must_use]
pub fn is_cli_binary() -> bool {
    exe_stem() == "mycode-cli"
}

/// What [`reconcile_pending_update`] found in the pending-update slot.
#[derive(Debug, PartialEq, Eq)]
pub enum PendingUpdate {
    /// No payload, or one dropped as superseded, corrupt, or failing.
    None,
    /// The payload's version is the one running now: the swap landed since
    /// the last launch, and the payload was cleaned up.
    Applied(String),
    /// The swap is armed and lands when this process exits.
    Armed(String),
}

/// Startup pass over this binary family's pending payload.
///
/// A payload left by a session that never finished its swap — a crash, a
/// rebooted helper, a lost race — is re-armed, so it still lands when
/// this process leaves (Unix swaps in place at once). A stale payload
/// (already applied, or superseded) and a corrupt one are deleted, and a
/// payload that failed too many startups is dropped so the app always
/// opens. The swap helper leaves the payload in place, so an applied one
/// is how the first launch of the new version learns the update landed.
pub fn reconcile_pending_update(home: &mycode_config::HomeLayout, relaunch: bool) -> PendingUpdate {
    if crate::install::install_skipped() {
        return PendingUpdate::None;
    }
    let dir = pending_dir(home);
    let stem = exe_stem();
    let (pending, sha_file, version_file, attempts_file) = pending_paths(&dir, &stem);
    let cleanup = |include_helpers: bool| {
        for path in [&pending, &sha_file, &version_file, &attempts_file] {
            let _ = std::fs::remove_file(path);
        }
        if include_helpers {
            sweep_update_helpers(&dir);
        }
    };
    if !pending.is_file() {
        // No payload: drop stray sidecars and dead helpers together.
        if version_file.exists() || sha_file.exists() || attempts_file.exists() {
            cleanup(true);
        }
        return PendingUpdate::None;
    }
    let version = std::fs::read_to_string(&version_file)
        .ok()
        .map(|value| value.trim().to_owned());
    let sha = std::fs::read_to_string(&sha_file)
        .ok()
        .map(|value| value.trim().to_owned());
    let (Some(version), Some(sha)) = (version, sha) else {
        cleanup(true);
        return PendingUpdate::None;
    };
    if !is_newer(&version, current_version()) {
        cleanup(true);
        return if version.trim_start_matches('v') == current_version() {
            PendingUpdate::Applied(version)
        } else {
            PendingUpdate::None
        };
    }
    let actual = file_sha256(&pending);
    if !actual.is_ok_and(|hash| hash.eq_ignore_ascii_case(&sha)) {
        cleanup(true);
        return PendingUpdate::None;
    }
    let attempts: u32 = std::fs::read_to_string(&attempts_file)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    if attempts >= MAX_PENDING_ATTEMPTS {
        cleanup(true);
        return PendingUpdate::None;
    }
    // Count before arming so a spawn that crashes still bounds the loop.
    let _ = std::fs::write(&attempts_file, (attempts + 1).to_string());
    let staged = PreparedUpdate {
        version: version.clone(),
        new_binary: pending,
        stage_dir: dir,
        binary_sha256: sha,
    };
    match arm_staged_update(&staged, relaunch) {
        Ok(()) => PendingUpdate::Armed(version),
        Err(_) => PendingUpdate::None,
    }
}

/// Removes dead update helpers and scripts inside [`pending_dir`]. A
/// helper whose process still runs is locked (Windows) or mid-flight
/// (Unix); removal failures are ignored either way.
fn sweep_update_helpers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let helper = name.starts_with("mycode-update-helper-") && name.ends_with(".exe");
        let script = name.starts_with("update-") && name.ends_with(".sh");
        // A `.moving` rename source is a persist interrupted mid-copy.
        let moving = name.ends_with(".moving");
        if helper || script || moving {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Removes stale staging directories left by interrupted updates.
pub fn cleanup_stale_stages() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with("mycode-update-") && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(not(windows))]
fn unix_script(new_binary: &Path, current: &Path, sha256: &str, relaunch: bool) -> String {
    let previous = previous_path(current);
    let previous = previous.to_string_lossy();
    let current = current.to_string_lossy();
    let new_binary = new_binary.to_string_lossy();
    // `shasum -a 256` is the hasher stock macOS ships. `sha256sum` is the
    // fallback for other Unix hosts. A failed backup stops the script; the
    // replace is not attempted without `.mycode-previous`. `cp -p` keeps the
    // mode, so a rollback puts an executable binary back. Once the replaced
    // binary verifies, the backup is deleted so old versions do not pile up
    // next to the install. Paths and the expected hash are single-quoted:
    // `$`, backticks, and `"` stay literal.
    let relaunch_line = if relaunch {
        "nohup \"$current\" >/dev/null 2>&1 &\n"
    } else {
        ""
    };
    let template = r#"#!/bin/sh
set -e
prev=__PREVIOUS__
current=__CURRENT__
new=__NEW__
expected=__HASH__
hash_file() {
  _file=$1
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$_file" | awk '{print $1}'
    return 0
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$_file" | awk '{print $1}'
    return 0
  fi
  echo "updater: neither shasum nor sha256sum is available" >&2
  exit 1
}
i=0
copied=0
actual=$(hash_file "$current")
if [ -n "$actual" ] && [ "$actual" = "$expected" ]; then
  # A sibling helper already landed the swap; only relaunch remains.
  rm -f "$new" 2>/dev/null || true
__RELAUNCH__
  rm -f "$0"
  exit 0
fi
actual=$(hash_file "$new")
if [ -z "$actual" ] || [ "$actual" != "$expected" ]; then
  echo "updater: staged binary sha256 does not match" >&2
  exit 1
fi
while [ "$i" -lt 30 ]; do
  if cp -p "$current" "$prev" 2>/dev/null; then
    copied=1
    break
  fi
  sleep 1
  i=$((i+1))
done
if [ "$copied" -ne 1 ]; then
  echo "updater: could not back up the current binary" >&2
  exit 1
fi
j=0
replaced=0
while [ "$j" -lt 30 ]; do
  if mv -f "$new" "$current" 2>/dev/null; then
    replaced=1
    break
  fi
  sleep 1
  j=$((j+1))
done
if [ "$replaced" -ne 1 ]; then
  actual=$(hash_file "$current")
  if [ -n "$actual" ] && [ "$actual" = "$expected" ]; then
    # A sibling helper landed the swap mid-flight; only relaunch remains.
    rm -f "$prev" 2>/dev/null || true
__RELAUNCH__
    rm -f "$0"
    exit 0
  fi
  echo "updater: could not replace the current binary" >&2
  exit 1
fi
actual=$(hash_file "$current")
if [ -z "$actual" ] || [ "$actual" != "$expected" ]; then
  if ! mv -f "$prev" "$current"; then
    echo "updater: replaced binary sha256 does not match; backup restore failed" >&2
    exit 1
  fi
  echo "updater: replaced binary sha256 does not match; restored backup" >&2
  exit 1
fi
chmod +x "$current" 2>/dev/null || true
rm -f "$prev"
__RELAUNCH__
rm -f "$0"
"#;
    template
        .replace("__PREVIOUS__", &shell_single_quote(&previous))
        .replace("__CURRENT__", &shell_single_quote(&current))
        .replace("__NEW__", &shell_single_quote(&new_binary))
        .replace("__HASH__", &shell_single_quote(sha256))
        .replace("__RELAUNCH__", relaunch_line)
}

fn previous_path(current: &Path) -> PathBuf {
    let file_name = current.file_name().unwrap_or(current.as_os_str());
    let mut name = file_name.to_os_string();
    name.push(".mycode-previous");
    current.with_file_name(name)
}

/// The staged file the Windows swap copies next to `target` before it is
/// renamed into place.
fn incoming_path(current: &Path) -> PathBuf {
    let file_name = current.file_name().unwrap_or(current.as_os_str());
    current.with_file_name(format!(".{}.mycode-incoming", file_name.to_string_lossy()))
}

/// Removes self-update leftovers next to the running executable.
///
/// The swap deletes `*.mycode-previous` after a verified replace and stages
/// `*.mycode-incoming` before every rename, and only relaunches the target
/// binary after verification has succeeded (no rollback ever runs after
/// relaunch). An interrupted helper, an older build, or a transient
/// remove lock can still leave either file behind, so normal UI startup
/// sweeps both paths immediately.
pub fn cleanup_previous_binary() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    for path in [previous_path(&exe), incoming_path(&exe)] {
        let Ok(meta) = path.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let _ = std::fs::remove_file(&path);
    }
}

/// Single-quote `text` for `/bin/sh`. `$`, backticks, `"`, and `\` stay
/// literal. An embedded `'` ends and reopens the quote.
#[cfg(not(windows))]
fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// SHA-256 of one file, streamed so a multi-hundred-megabyte asset never
/// sits whole in memory. The digest is lowercase hex.
fn file_sha256(path: &Path) -> Result<String, String> {
    Ok(hex_encode(&file_sha256_raw(path)?))
}

fn file_sha256_raw(path: &Path) -> Result<[u8; 32], String> {
    let mut file = std::fs::File::open(path).map_err(|error| format!("hash read: {error}"))?;
    let mut hasher = sha2::Sha256::new();
    // Chunked reads: the digest crate's hasher does not implement
    // `io::Write` here, and reading the whole file would buffer a
    // multi-hundred-megabyte asset in memory just to hash it.
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("hash read: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

/// Lowercase hex without a `format!` allocation per byte.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Lowercase hex of one in-memory byte slice. Test fixtures build expected
/// digests from it; production hashes files through [`file_sha256`].
#[cfg(test)]
fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&sha2::Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;
