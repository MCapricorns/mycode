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
/// Published archives are Windows x64, Windows ARM64, and macOS Apple
/// Silicon. Linux, Intel macOS, Windows x86, and every other target resolve
/// no asset and stay on their installed version.
#[must_use]
pub fn asset_suffix() -> &'static str {
    asset_suffix_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn asset_suffix_for(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("windows", "x86_64") => "-x86_64-pc-windows-msvc.zip",
        ("windows", "aarch64") => "-aarch64-pc-windows-msvc.zip",
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
    #[serde(default)]
    size: u64,
}

/// Resolves the latest release; `Ok(None)` means the app is current.
///
/// # Errors
///
/// Returns the transport, parse, version-parse, or HTTP failure message.
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
        user_agent: Some(user_agent.to_owned()),
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
    user_agent: &str,
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
    fetch_to_file(user_agent, &offer.asset_url, &asset_path, MAX_ASSET_BYTES).await?;

    let checksum_path = stage_dir.join("update.sha256");
    fetch_to_file(
        user_agent,
        &offer.checksum_url,
        &checksum_path,
        MAX_CHECKSUM_BYTES,
    )
    .await?;
    verify_checksum(&asset_path, &checksum_path)?;

    let binary_path = extract_binary(&asset_path, &stage_dir)?;
    let binary_sha256 = file_sha256(&binary_path)?;
    let _ = std::fs::remove_file(&asset_path);
    let _ = std::fs::remove_file(&checksum_path);
    Ok(PreparedUpdate {
        new_binary: binary_path,
        stage_dir,
        binary_sha256,
    })
}

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
        timeout: Some(std::time::Duration::from_secs(30)),
        user_agent: Some(user_agent.to_owned()),
        cancel: tokio_util::sync::CancellationToken::new(),
    })
    .await
    .map_err(|error| format!("download failed: {}", brief_error(&error)))?;
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
    let actual = file_sha256(&prepared.new_binary)?;
    if !actual.eq_ignore_ascii_case(&prepared.binary_sha256) {
        return Err("staged update binary failed its sha256 check".to_owned());
    }
    let current = std::env::current_exe().map_err(|error| format!("current exe: {error}"))?;
    let current = current.canonicalize().unwrap_or(current);
    let script_path = prepared.stage_dir.join(if cfg!(windows) {
        "update.cmd"
    } else {
        "update.sh"
    });
    let script = if cfg!(windows) {
        windows_script(
            &prepared.new_binary,
            &current,
            &prepared.binary_sha256,
            true,
        )
    } else {
        unix_script(
            &prepared.new_binary,
            &current,
            &prepared.binary_sha256,
            true,
        )
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

fn windows_script(new_binary: &Path, current: &Path, sha256: &str, relaunch: bool) -> String {
    let previous = previous_path(current);
    let previous = previous.to_string_lossy();
    let current = current.to_string_lossy();
    let new_binary = new_binary.to_string_lossy();
    // `timeout` returns immediately when cmd has no console. The updater is
    // spawned detached, so the wait is `ping`, which still sleeps about a
    // second. The retry covers the replace, not only the backup copy: a
    // running image can be copied and still refuse to be renamed.
    //
    // Paths are stored while delayed expansion is off, with `%` doubled so
    // the stored value keeps a literal percent. Later references use `!VAR!`,
    // which does not expand `%VAR%` inside the value. `for /f` runs a second
    // batch file: re-entering this file makes cmd lose its place and report
    // "The batch file cannot be found." The helper's certutil line is a
    // normal line, so quotes, `)`, and `%` are not parsed by `for /f`.
    let relaunch_line = if relaunch {
        "start \"\" \"!CURRENT!\"\n"
    } else {
        ""
    };
    let template = r#"@echo off
setlocal EnableExtensions DisableDelayedExpansion
set "TRIES=0"
set "BACKED_UP=0"
set "CURRENT=__CURRENT__"
set "PREVIOUS=__PREVIOUS__"
set "NEW=__NEW__"
set "EXPECTED=__HASH__"
setlocal EnableDelayedExpansion
:retry
if "!BACKED_UP!"=="0" goto backup
goto replace
:backup
copy /y "!CURRENT!" "!PREVIOUS!" >nul 2>&1
if errorlevel 1 goto wait
set "BACKED_UP=1"
:replace
set "MYCODE_HASH_TARGET=!NEW!"
call :checkhash
if errorlevel 1 (
  echo updater: staged binary sha256 does not match 1>&2
  exit /b 1
)
move /y "!NEW!" "!CURRENT!" >nul 2>&1
if errorlevel 1 goto wait
set "MYCODE_HASH_TARGET=!CURRENT!"
call :checkhash
if errorlevel 1 goto rollback
goto done
:wait
set /a TRIES+=1
if !TRIES! GEQ 30 (
  if "!BACKED_UP!"=="0" (
    echo updater: could not back up the current binary 1>&2
  ) else (
    echo updater: could not replace the current binary before the retry limit 1>&2
  )
  exit /b 1
)
ping -n 2 127.0.0.1 >nul
goto retry
:rollback
echo updater: replaced binary sha256 does not match; restoring backup 1>&2
move /y "!PREVIOUS!" "!CURRENT!" >nul 2>&1
exit /b 1
:done
__RELAUNCH__
rem Same line as the delete: cmd has already read it, so removing this
rem script does not hide `exit` and turn a good replace into errorlevel 1.
del "%~f0" & exit /b 0
:checkhash
set "HASHRESULT="
set "SEEN="
rem The for-variable is %%L. A name that starts with that letter, such as
rem %LINE%, is parsed as the for-variable plus leftover text.
set "HASHCMD=%~dp0mycode-hash.cmd"
setlocal DisableDelayedExpansion
> "%HASHCMD%" echo @echo off
>> "%HASHCMD%" echo setlocal EnableExtensions EnableDelayedExpansion
>> "%HASHCMD%" echo certutil -hashfile "!MYCODE_HASH_TARGET!" SHA256
endlocal
if not exist "%HASHCMD%" (
  echo updater: missing helper !HASHCMD! 1>&2
  exit /b 1
)
for /f "usebackq delims=" %%L in (`call "%HASHCMD%"`) do (
  if not defined SEEN set "SEEN=%%L"
  set "CANDIDATE=%%L"
  set "CANDIDATE=!CANDIDATE: =!"
  if not defined HASHRESULT if "!CANDIDATE:~64,1!"=="" if not "!CANDIDATE:~63,1!"=="" set "HASHRESULT=!CANDIDATE!"
)
del "%HASHCMD%" >nul 2>&1
if not defined HASHRESULT (
  echo updater: certutil did not return a sha256 [!SEEN!] target [!MYCODE_HASH_TARGET!] 1>&2
  exit /b 1
)
if /i not "!HASHRESULT!"=="!EXPECTED!" (
  echo updater: sha256 !HASHRESULT! does not match !EXPECTED! 1>&2
  exit /b 1
)
exit /b 0
"#;
    template
        .replace("__PREVIOUS__", &cmd_percent_escape(&previous))
        .replace("__CURRENT__", &cmd_percent_escape(&current))
        .replace("__NEW__", &cmd_percent_escape(&new_binary))
        .replace("__HASH__", &cmd_percent_escape(sha256))
        .replace("__RELAUNCH__", relaunch_line)
        .replace('\n', "\r\n")
}

fn unix_script(new_binary: &Path, current: &Path, sha256: &str, relaunch: bool) -> String {
    let previous = previous_path(current);
    let previous = previous.to_string_lossy();
    let current = current.to_string_lossy();
    let new_binary = new_binary.to_string_lossy();
    // `shasum -a 256` is the hasher stock macOS ships. `sha256sum` is the
    // fallback for other Unix hosts. A failed backup stops the script; the
    // replace is not attempted without `.mycode-previous`. `cp -p` keeps the
    // mode, so a rollback puts an executable binary back. Paths and the
    // expected hash are single-quoted: `$`, backticks, and `"` stay literal.
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
actual=$(hash_file "$new")
if [ -z "$actual" ] || [ "$actual" != "$expected" ]; then
  echo "updater: staged binary sha256 does not match" >&2
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

/// Single-quote `text` for `/bin/sh`. `$`, backticks, `"`, and `\` stay
/// literal. An embedded `'` ends and reopens the quote.
fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Double `%` so a batch file stores a literal percent instead of expanding
/// an environment variable. The value is placed inside `set "VAR=..."`.
fn cmd_percent_escape(text: &str) -> String {
    text.replace('%', "%%")
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("hash read: {error}"))?;
    Ok(sha256_hex(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
            asset_suffix_for("macos", "aarch64"),
            "-aarch64-apple-darwin.zip"
        );
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mycode-update-script-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn output_text(output: &std::process::Output) -> String {
        format!(
            "status {:?}\nstdout {}\nstderr {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn wait_child(
        child: &mut std::process::Child,
        timeout: std::time::Duration,
    ) -> std::process::Output {
        use std::io::Read as _;
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                panic!("updater script timed out after {}s", timeout.as_secs());
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut stdout = Vec::new();
                    let mut stderr = Vec::new();
                    if let Some(mut pipe) = child.stdout.take() {
                        pipe.read_to_end(&mut stdout).unwrap();
                    }
                    if let Some(mut pipe) = child.stderr.take() {
                        pipe.read_to_end(&mut stderr).unwrap();
                    }
                    return std::process::Output {
                        status,
                        stdout,
                        stderr,
                    };
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                Err(error) => panic!("wait: {error}"),
            }
        }
    }

    #[test]
    fn relaunch_is_a_script_parameter() {
        let staged = std::path::Path::new("/tmp/next");
        let current = std::path::Path::new("/tmp/app");
        let quiet = super::unix_script(staged, current, "ab", false);
        assert!(!quiet.contains("nohup"), "{quiet}");
        let live = super::unix_script(staged, current, "ab", true);
        assert!(live.contains("nohup \"$current\""), "{live}");
        let quiet = super::windows_script(staged, current, "ab", false);
        assert!(!quiet.contains("start \"\""), "{quiet}");
        let live = super::windows_script(staged, current, "ab", true);
        assert!(live.contains("start \"\" \"!CURRENT!\""), "{live}");
    }

    #[test]
    fn windows_script_percent_signs_survive_cmd_parsing() {
        let current = std::path::Path::new(r"C:\updates\%SystemRoot%\app.exe");
        let staged = std::path::Path::new(r"C:\updates\%SystemRoot%\next.exe");
        let script = super::windows_script(staged, current, "abc", false);
        assert!(
            script.contains("set \"CURRENT=C:\\updates\\%%SystemRoot%%\\app.exe\""),
            "{script}"
        );
        assert!(
            script.contains("set \"MYCODE_HASH_TARGET=!NEW!\""),
            "{script}"
        );
        assert!(
            script.contains("set \"HASHCMD=%~dp0mycode-hash.cmd\""),
            "{script}"
        );
        assert!(script.contains("call \"%HASHCMD%\""), "{script}");
        assert!(
            script.contains("certutil -hashfile \"!MYCODE_HASH_TARGET!\" SHA256"),
            "{script}"
        );
    }

    #[cfg(unix)]
    fn unix_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_unix_mode(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    struct DirModeGuard<'a>(&'a std::path::Path);

    #[cfg(unix)]
    impl Drop for DirModeGuard<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    #[cfg(unix)]
    fn run_sh(script: &std::path::Path) -> std::process::Output {
        std::process::Command::new("/bin/sh")
            .arg(script)
            .output()
            .unwrap()
    }

    #[cfg(unix)]
    fn spawn_sh(script: &std::path::Path) -> std::process::Child {
        std::process::Command::new("/bin/sh")
            .arg(script)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn unix_script_good_hash_replaces_and_keeps_backup() {
        let dir = scratch("unix-good");
        let current = dir.join("app");
        let staged = dir.join("next");
        let old = b"old-bytes";
        let new = b"new-bytes";
        std::fs::write(&current, old).unwrap();
        set_unix_mode(&current, 0o755);
        std::fs::write(&staged, new).unwrap();
        let hash = super::sha256_hex(new);
        let script = super::unix_script(&staged, &current, &hash, false);
        assert!(
            script.contains("shasum -a 256"),
            "macOS hasher missing from shipped script: {script}"
        );
        assert!(script.contains("cp -p"), "{script}");
        let path = dir.join("update.sh");
        std::fs::write(&path, &script).unwrap();
        let output = run_sh(&path);
        assert!(
            output.status.success(),
            "good hash should replace: {}",
            output_text(&output)
        );
        assert_eq!(std::fs::read(&current).unwrap(), new);
        let backup = super::previous_path(&current);
        assert_eq!(std::fs::read(&backup).unwrap(), old);
        assert_eq!(
            unix_mode(&backup),
            0o755,
            "backup must keep the executable mode"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn unix_script_bad_hash_leaves_current_untouched() {
        let dir = scratch("unix-bad");
        let current = dir.join("app");
        let staged = dir.join("next");
        std::fs::write(&current, b"old-bytes").unwrap();
        std::fs::write(&staged, b"new-bytes").unwrap();
        let script = super::unix_script(
            &staged,
            &current,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            false,
        );
        let path = dir.join("update.sh");
        std::fs::write(&path, script).unwrap();
        let output = run_sh(&path);
        let text = output_text(&output);
        assert!(!output.status.success(), "bad hash must fail: {text}");
        assert!(
            text.contains("sha256"),
            "hash failure must be visible: {text}"
        );
        assert_eq!(std::fs::read(&current).unwrap(), b"old-bytes");
        assert_eq!(std::fs::read(&staged).unwrap(), b"new-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn unix_script_quotes_metacharacters_in_paths_and_hash() {
        let dir = scratch("unix-quote");
        let name = "pay$me$(touch pwned)`touch pwned2`\"q'ote";
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let current = root.join("app");
        let staged = root.join("next");
        std::fs::write(&current, b"old-bytes").unwrap();
        std::fs::write(&staged, b"new-bytes").unwrap();
        let script = super::unix_script(&staged, &current, "a'$(touch pwned3)", false);
        let path = dir.join("update.sh");
        std::fs::write(&path, script).unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg(&path)
            .current_dir(&dir)
            .output()
            .unwrap();
        let text = output_text(&output);
        assert!(!output.status.success(), "evil hash must fail: {text}");
        assert!(!dir.join("pwned").exists(), "path expanded: {text}");
        assert!(!dir.join("pwned2").exists(), "backticks ran: {text}");
        assert!(!dir.join("pwned3").exists(), "hash expanded: {text}");
        assert_eq!(std::fs::read(&current).unwrap(), b"old-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn unix_script_retries_replace_while_target_is_busy() {
        let dir = scratch("unix-busy");
        let current = dir.join("app");
        let staged = dir.join("next");
        let backup = super::previous_path(&current);
        std::fs::write(&current, b"old-bytes").unwrap();
        set_unix_mode(&current, 0o755);
        std::fs::write(&staged, b"new-bytes").unwrap();
        // The backup already exists so `cp -p` can overwrite it without
        // creating a directory entry. The directory itself is not writable,
        // which makes `mv` fail until the test releases it.
        std::fs::write(&backup, b"stale").unwrap();
        let script = super::unix_script(&staged, &current, &super::sha256_hex(b"new-bytes"), false);
        let path = dir.join("update.sh");
        std::fs::write(&path, &script).unwrap();
        set_unix_mode(&dir, 0o555);
        let _restore_dir = DirModeGuard(&dir);
        let mut child = spawn_sh(&path);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert_eq!(
            std::fs::read(&current).unwrap(),
            b"old-bytes",
            "replace succeeded while the directory rejected mv"
        );
        assert_eq!(std::fs::read(&backup).unwrap(), b"old-bytes");
        set_unix_mode(&dir, 0o755);
        let output = wait_child(&mut child, std::time::Duration::from_secs(15));
        assert!(
            output.status.success(),
            "replace should succeed after the target is free: {}",
            output_text(&output)
        );
        assert_eq!(std::fs::read(&current).unwrap(), b"new-bytes");
        assert_eq!(std::fs::read(&backup).unwrap(), b"old-bytes");
        assert_eq!(unix_mode(&backup), 0o755);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn unix_script_post_replace_mismatch_restores_executable_backup() {
        let dir = scratch("unix-mismatch");
        let current = dir.join("app");
        let staged = dir.join("next");
        std::fs::write(&current, b"old-bytes").unwrap();
        set_unix_mode(&current, 0o755);
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&staged)
                .status()
                .unwrap()
                .success(),
            "mkfifo"
        );
        let script = super::unix_script(&staged, &current, &super::sha256_hex(b"new-bytes"), false);
        let path = dir.join("update.sh");
        std::fs::write(&path, &script).unwrap();
        let mut child = spawn_sh(&path);
        let staged_writer = staged.clone();
        let first = std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&staged_writer)
                .unwrap();
            use std::io::Write as _;
            file.write_all(b"new-bytes").unwrap();
        });
        let start = std::time::Instant::now();
        let mut saw_fifo = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            saw_fifo = std::fs::metadata(&current)
                .map(|metadata| {
                    use std::os::unix::fs::FileTypeExt as _;
                    metadata.file_type().is_fifo()
                })
                .unwrap_or(false);
            if saw_fifo {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(saw_fifo, "staged fifo was not moved onto the current path");
        let current_writer = current.clone();
        let second = std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&current_writer)
                .unwrap();
            use std::io::Write as _;
            file.write_all(b"corrupt").unwrap();
        });
        let output = wait_child(&mut child, std::time::Duration::from_secs(10));
        first.join().unwrap();
        second.join().unwrap();
        let text = output_text(&output);
        assert!(!output.status.success(), "mismatch must fail: {text}");
        assert!(
            text.contains("restored"),
            "rollback must be visible: {text}"
        );
        assert_eq!(std::fs::read(&current).unwrap(), b"old-bytes");
        assert_eq!(unix_mode(&current), 0o755);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn unix_script_stops_when_backup_cannot_be_created() {
        let dir = scratch("unix-nobackup");
        let current = dir.join("missing");
        let staged = dir.join("next");
        std::fs::write(&staged, b"new-bytes").unwrap();
        let script = super::unix_script(&staged, &current, &super::sha256_hex(b"new-bytes"), false);
        let path = dir.join("update.sh");
        std::fs::write(&path, script).unwrap();
        let output = run_sh(&path);
        let text = output_text(&output);
        assert!(!output.status.success(), "missing backup must fail: {text}");
        assert!(
            text.contains("back up"),
            "backup failure must be visible: {text}"
        );
        assert!(!current.exists(), "script replaced without a backup");
        assert_eq!(std::fs::read(&staged).unwrap(), b"new-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    fn run_cmd(script: &std::path::Path) -> std::process::Output {
        let script = script.to_string_lossy().into_owned();
        std::process::Command::new("cmd")
            .args(["/d", "/c", script.as_str()])
            .output()
            .unwrap()
    }

    #[cfg(windows)]
    fn spawn_cmd(script: &std::path::Path) -> std::process::Child {
        let script = script.to_string_lossy().into_owned();
        std::process::Command::new("cmd")
            .args(["/d", "/c", script.as_str()])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Holds `path` so another process can read it but cannot rename it.
    #[cfg(windows)]
    fn lock_against_replace(path: &std::path::Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn windows_script_good_hash_replaces_when_target_is_not_running() {
        let dir = scratch("win-good");
        let current = dir.join("app.exe");
        let staged = dir.join("next.exe");
        let new = b"new-bytes";
        std::fs::write(&current, b"old-bytes").unwrap();
        std::fs::write(&staged, new).unwrap();
        let script = super::windows_script(&staged, &current, &super::sha256_hex(new), false);
        let path = dir.join("update.cmd");
        std::fs::write(&path, script).unwrap();
        let output = run_cmd(&path);
        assert!(
            output.status.success(),
            "good hash should replace: {}",
            output_text(&output)
        );
        assert_eq!(std::fs::read(&current).unwrap(), new);
        let backup = super::previous_path(&current);
        assert_eq!(std::fs::read(&backup).unwrap(), b"old-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn windows_script_bad_hash_leaves_current_untouched() {
        let dir = scratch("win-bad");
        let current = dir.join("app.exe");
        let staged = dir.join("next.exe");
        std::fs::write(&current, b"old-bytes").unwrap();
        std::fs::write(&staged, b"new-bytes").unwrap();
        let script = super::windows_script(
            &staged,
            &current,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            false,
        );
        let path = dir.join("update.cmd");
        std::fs::write(&path, script).unwrap();
        let output = run_cmd(&path);
        let text = output_text(&output);
        assert!(!output.status.success(), "bad hash must fail: {text}");
        assert!(
            text.contains("sha256"),
            "hash failure must be visible: {text}"
        );
        assert_eq!(std::fs::read(&current).unwrap(), b"old-bytes");
        assert_eq!(std::fs::read(&staged).unwrap(), b"new-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn windows_script_percent_in_path_is_literal() {
        let dir = scratch("win-pct");
        let root = dir.join("%SystemRoot%_%PATH%");
        std::fs::create_dir_all(&root).unwrap();
        let current = root.join("app.exe");
        let staged = root.join("next.exe");
        std::fs::write(&current, b"old-bytes").unwrap();
        std::fs::write(&staged, b"new-bytes").unwrap();
        let script =
            super::windows_script(&staged, &current, &super::sha256_hex(b"new-bytes"), false);
        let path = dir.join("update.cmd");
        std::fs::write(&path, &script).unwrap();
        let output = run_cmd(&path);
        assert!(
            output.status.success(),
            "percent in the path must stay literal: {}\n{script}",
            output_text(&output)
        );
        assert_eq!(std::fs::read(&current).unwrap(), b"new-bytes");
        assert_eq!(
            std::fs::read(super::previous_path(&current)).unwrap(),
            b"old-bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn windows_script_retries_replace_while_target_is_busy() {
        let dir = scratch("win-busy");
        let current = dir.join("app.exe");
        let staged = dir.join("next.exe");
        std::fs::write(&current, b"old-bytes").unwrap();
        std::fs::write(&staged, b"new-bytes").unwrap();
        let script =
            super::windows_script(&staged, &current, &super::sha256_hex(b"new-bytes"), false);
        let path = dir.join("update.cmd");
        std::fs::write(&path, script).unwrap();
        let lock = lock_against_replace(&current);
        let mut child = spawn_cmd(&path);
        std::thread::sleep(std::time::Duration::from_millis(2000));
        let backup = super::previous_path(&current);
        assert_eq!(
            std::fs::read(&current).unwrap(),
            b"old-bytes",
            "replace succeeded while the target was locked"
        );
        assert_eq!(std::fs::read(&backup).unwrap(), b"old-bytes");
        drop(lock);
        let output = wait_child(&mut child, std::time::Duration::from_secs(20));
        assert!(
            output.status.success(),
            "replace should succeed after the lock is released: {}",
            output_text(&output)
        );
        assert_eq!(std::fs::read(&current).unwrap(), b"new-bytes");
        assert_eq!(std::fs::read(&backup).unwrap(), b"old-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn windows_script_post_replace_mismatch_restores_backup() {
        let dir = scratch("win-mismatch");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let count = dir.join("certutil-count.txt");
        // The helper runs `certutil` with no extension, so PATH finds
        // certutil.cmd before System32's certutil.exe. The first call
        // hashes for real; the second prints a different hash.
        let wrapper = format!(
            "@echo off\r\nsetlocal EnableExtensions\r\nset /a N=0\r\nif exist \"{count}\" set /p N=<\"{count}\"\r\nset /a N+=1\r\n>\"{count}\" echo %N%\r\nif %N% GEQ 2 (\r\n  echo SHA256 hash of dummy:\r\n  echo ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\r\n  echo CertUtil: -hashfile command completed successfully.\r\n  exit /b 0\r\n)\r\n\"%SystemRoot%\\System32\\certutil.exe\" %*\r\nexit /b %ERRORLEVEL%\r\n",
            count = count.display()
        );
        std::fs::write(bin.join("certutil.cmd"), wrapper).unwrap();
        let current = dir.join("app.exe");
        let staged = dir.join("next.exe");
        std::fs::write(&current, b"old-bytes").unwrap();
        std::fs::write(&staged, b"new-bytes").unwrap();
        let script =
            super::windows_script(&staged, &current, &super::sha256_hex(b"new-bytes"), false);
        let path = dir.join("update.cmd");
        std::fs::write(&path, script).unwrap();
        let path_text = path.to_string_lossy().into_owned();
        let path_env = format!(
            "{};{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = std::process::Command::new("cmd")
            .args(["/d", "/c", path_text.as_str()])
            .env("PATH", path_env)
            .output()
            .unwrap();
        let text = output_text(&output);
        assert!(!output.status.success(), "mismatch must fail: {text}");
        assert!(text.contains("restor"), "rollback must be visible: {text}");
        assert_eq!(std::fs::read(&current).unwrap(), b"old-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_platforms_have_no_asset() {
        assert_eq!(asset_suffix_for("linux", "x86_64"), "");
        assert_eq!(asset_suffix_for("macos", "x86_64"), "");
        assert_eq!(asset_suffix_for("linux", "aarch64"), "");
        assert_eq!(asset_suffix_for("windows", "x86"), "");
        assert_eq!(asset_suffix_for("freebsd", "x86_64"), "");
    }
}
