//! GitHub-release based self-update for the MYCode desktop application.
//!
//! The checker resolves the latest published release, picks the release
//! asset matching the running platform, and compares semantic versions. The
//! installer downloads the asset plus its `.sha256` sidecar, verifies the
//! digest, extracts the new binary into a staging directory, and hands the
//! actual swap to a small detached updater script so the running process can
//! exit first. Asset names and staging conventions are produced by the
//! release jobs in `.github/workflows/ci.yml`.
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use sha2::Digest as _;

/// GitHub API endpoint resolving the latest published release.
pub const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/MCapricorns/mycode/releases/latest";

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

/// Release asset suffix for the running platform, empty when unsupported.
///
/// Published archives are Windows x64, Windows ARM64, Linux x86_64 GNU, and
/// macOS Apple Silicon. Intel macOS, Windows x86, and every other target,
/// including Linux aarch64, resolve no asset and stay on their installed
/// version.
/// `std::env::consts` does not distinguish glibc from musl; the Linux x86_64
/// suffix is the published GNU archive.
#[must_use]
pub fn asset_suffix() -> &'static str {
    asset_suffix_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn asset_suffix_for(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("windows", "x86_64") => "-x86_64-pc-windows-msvc.zip",
        ("windows", "aarch64") => "-aarch64-pc-windows-msvc.zip",
        ("linux", "x86_64") => "-x86_64-unknown-linux-gnu.zip",
        ("macos", "aarch64") => "-aarch64-apple-darwin.zip",
        _ => "",
    }
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
    /// Asset size in bytes.
    pub asset_size: u64,
    /// Checksum sidecar URL.
    pub checksum_url: String,
}

/// A downloaded and verified update, staged for the swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedUpdate {
    /// Staged new binary, ready to replace the running executable.
    pub new_binary: PathBuf,
    /// Staging directory; removable on the next startup.
    pub stage_dir: PathBuf,
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
    #[serde(default)]
    size: u64,
}

/// Resolves the latest release; `Ok(None)` means the app is current.
///
/// # Errors
///
/// Returns the transport, parse, version-parse, or HTTP failure message.
pub async fn latest_release(client: &reqwest::Client) -> Result<Option<UpdateOffer>, String> {
    let response = client
        .get(LATEST_RELEASE_API)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("update check failed: {}", brief_error(&error.to_string())))?;
    if classify_release_status(response.status())?.is_none() {
        return Ok(None);
    }
    let release: ReleaseJson = response.json().await.map_err(|error| {
        format!(
            "update response malformed: {}",
            brief_error(&error.to_string())
        )
    })?;
    let Some(offer) = resolve_asset(&release.tag_name, &release.html_url, &release.assets) else {
        return Ok(None);
    };
    if !is_newer(&release.tag_name, current_version()) {
        return Ok(None);
    }
    Ok(Some(offer))
}

/// First line of a transport error, bounded, so reqwest's full error chain
/// (URL,TLS, and retry diagnostics) cannot stretch a toast across the screen.
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

fn resolve_asset(tag: &str, notes_url: &str, assets: &[AssetJson]) -> Option<UpdateOffer> {
    let suffix = asset_suffix();
    if suffix.is_empty() {
        return None;
    }
    let asset = assets
        .iter()
        .find(|asset| asset.name.starts_with("mycode-desktop-") && asset.name.ends_with(suffix))?;
    let version = tag.trim_start_matches('v').to_owned();
    Some(UpdateOffer {
        version,
        notes_url: notes_url.to_owned(),
        asset_url: asset.browser_download_url.clone(),
        asset_size: asset.size,
        checksum_url: format!("{}.sha256", asset.browser_download_url),
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
pub async fn download_update(
    client: &reqwest::Client,
    offer: &UpdateOffer,
) -> Result<PreparedUpdate, String> {
    let stage_dir = std::env::temp_dir().join(format!(
        "mycode-update-{}-{}",
        std::process::id(),
        offer.version.replace('.', "-")
    ));
    if stage_dir.exists() {
        std::fs::remove_dir_all(&stage_dir).map_err(|error| format!("stage reset: {error}"))?;
    }
    std::fs::create_dir_all(&stage_dir).map_err(|error| format!("stage create: {error}"))?;

    let asset_path = stage_dir.join("update.asset");
    fetch_to_file(client, &offer.asset_url, &asset_path, MAX_ASSET_BYTES).await?;

    let checksum_path = stage_dir.join("update.sha256");
    fetch_to_file(
        client,
        &offer.checksum_url,
        &checksum_path,
        MAX_CHECKSUM_BYTES,
    )
    .await?;
    verify_checksum(&asset_path, &checksum_path)?;

    let binary_path = extract_binary(&asset_path, &stage_dir)?;
    let _ = std::fs::remove_file(&asset_path);
    let _ = std::fs::remove_file(&checksum_path);
    Ok(PreparedUpdate {
        new_binary: binary_path,
        stage_dir,
    })
}

async fn fetch_to_file(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    maximum: u64,
) -> Result<(), String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("download failed: {}", brief_error(&error.to_string())))?;
    if !response.status().is_success() {
        return Err(format!("download returned {}", response.status()));
    }
    let file = std::fs::File::create(destination).map_err(|error| format!("stage: {error}"))?;
    let mut writer = std::io::BufWriter::new(file);
    let mut downloaded: u64 = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("download stream failed: {error}"))?
    {
        downloaded += chunk.len() as u64;
        if downloaded > maximum {
            return Err("download exceeded its size bound".to_owned());
        }
        writer
            .write_all(&chunk)
            .map_err(|error| format!("stage write failed: {error}"))?;
    }
    writer
        .flush()
        .map_err(|error| format!("stage write failed: {error}"))?;
    Ok(())
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
    let bytes = std::fs::read(asset).map_err(|error| format!("asset read: {error}"))?;
    let digest = sha2::Sha256::digest(&bytes);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual != expected {
        return Err("downloaded update failed its checksum verification".to_owned());
    }
    Ok(())
}

fn extract_binary(asset: &Path, stage_dir: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(asset).map_err(|error| format!("asset open: {error}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| format!("asset open: {error}"))?;
    let mut entry_name: Option<String> = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("asset entry: {error}"))?;
        let name = entry.name().to_owned();
        if entry.is_dir() {
            continue;
        }
        let base = name.split('/').next_back().unwrap_or(&name);
        let executable = name.ends_with(".exe") || base == "mycode-desktop";
        if !executable {
            continue;
        }
        let mut parts = std::path::Path::new(&name).components();
        let malicious = name.contains('\\')
            || parts.any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::RootDir
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
        .unwrap_or("mycode-desktop")
        .to_owned();
    let binary_path = stage_dir.join(&base_name);
    let mut entry = archive
        .by_name(&entry_name)
        .map_err(|error| format!("asset entry: {error}"))?;
    let mut out =
        std::fs::File::create(&binary_path).map_err(|error| format!("stage binary: {error}"))?;
    std::io::copy(&mut entry, &mut out).map_err(|error| format!("stage binary: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("stage binary: {error}"))?;
    }
    Ok(binary_path)
}

/// Arms the detached swap script and returns; the caller exits the process
/// so the updater can replace the binary and relaunch it.
///
/// # Errors
///
/// Returns a failure message when the script or its process cannot start.
pub fn apply_and_restart(prepared: &PreparedUpdate) -> Result<(), String> {
    let current = std::env::current_exe().map_err(|error| format!("current exe: {error}"))?;
    let current = current.canonicalize().unwrap_or(current);
    let script_path = prepared.stage_dir.join(if cfg!(windows) {
        "update.cmd"
    } else {
        "update.sh"
    });
    let script = if cfg!(windows) {
        windows_script(&prepared.new_binary, &current)
    } else {
        unix_script(&prepared.new_binary, &current)
    };
    std::fs::write(&script_path, script).map_err(|error| format!("updater script: {error}"))?;

    #[cfg(windows)]
    spawn_windows_updater(&script_path)?;
    #[cfg(not(windows))]
    Command::new("/bin/sh")
        .arg(&script_path)
        .spawn()
        .map_err(|error| format!("updater spawn: {error}"))?;
    Ok(())
}

#[cfg(windows)]
fn spawn_windows_updater(script_path: &Path) -> Result<(), String> {
    // 0x00000008 DETACHED_PROCESS | 0x08000000 CREATE_NO_WINDOW: the swap
    // must outlive this process and never flash a console.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt as _;
    Command::new("cmd")
        .args(["/c", &script_path.to_string_lossy()])
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| format!("updater spawn: {error}"))?;
    Ok(())
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

fn windows_script(new_binary: &Path, current: &Path) -> String {
    format!(
        "@echo off\r\nset /a TRIES=0\r\n:retry\r\ntimeout /t 1 /nobreak >nul\r\nmove /y \"{}\" \"{}\" >nul 2>&1\r\nif not errorlevel 1 goto done\r\nset /a TRIES+=1\r\nif %TRIES% GEQ 30 exit /b 1\r\ngoto retry\r\n:done\r\nstart \"\" \"{}\"\r\ndel \"%~f0\"\r\n",
        new_binary.display(),
        current.display(),
        current.display(),
    )
}

fn unix_script(new_binary: &Path, current: &Path) -> String {
    format!(
        "#!/bin/sh\ni=0\nwhile [ $i -lt 30 ]; do\n  if mv -f \"{}\" \"{}\" 2>/dev/null; then break; fi\n  sleep 1\n  i=$((i+1))\ndone\nchmod +x \"{}\" 2>/dev/null\nnohup \"{}\" >/dev/null 2>&1 &\nrm -f \"$0\"\n",
        new_binary.display(),
        current.display(),
        current.display(),
        current.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::asset_suffix_for;

    #[test]
    fn published_platforms_match_release_assets() {
        assert_eq!(
            asset_suffix_for("windows", "x86_64"),
            "-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            asset_suffix_for("windows", "aarch64"),
            "-aarch64-pc-windows-msvc.zip"
        );
        assert_eq!(
            asset_suffix_for("linux", "x86_64"),
            "-x86_64-unknown-linux-gnu.zip"
        );
        assert_eq!(
            asset_suffix_for("macos", "aarch64"),
            "-aarch64-apple-darwin.zip"
        );
    }

    #[test]
    fn unsupported_platforms_have_no_asset() {
        assert_eq!(asset_suffix_for("macos", "x86_64"), "");
        assert_eq!(asset_suffix_for("linux", "aarch64"), "");
        assert_eq!(asset_suffix_for("windows", "x86"), "");
        assert_eq!(asset_suffix_for("freebsd", "x86_64"), "");
    }
}
