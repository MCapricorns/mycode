use super::asset_suffix_for;

const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[test]
fn checksum_accepts_sha256_abc_known_vector() {
    let dir = scratch("checksum-abc");
    let asset = dir.join("update.asset");
    let checksum = dir.join("update.sha256");
    std::fs::write(&asset, b"abc").unwrap();
    for sidecar in [
        ABC_SHA256.to_owned(),
        format!("{ABC_SHA256}  update.asset\n"),
        format!("{} *update.asset\n", ABC_SHA256.to_ascii_uppercase()),
    ] {
        std::fs::write(&checksum, sidecar).unwrap();
        assert_eq!(super::verify_checksum(&asset, &checksum), Ok(()));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn checksum_rejects_tampered_asset() {
    let dir = scratch("checksum-tampered");
    let asset = dir.join("update.asset");
    let checksum = dir.join("update.sha256");
    std::fs::write(&asset, b"abd").unwrap();
    std::fs::write(&checksum, format!("{ABC_SHA256}  update.asset\n")).unwrap();
    let result = super::verify_checksum(&asset, &checksum);
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(
        result,
        Err("downloaded update failed its checksum verification".to_owned())
    );
}

#[test]
fn checksum_rejects_malformed_sidecars() {
    let dir = scratch("checksum-malformed");
    let asset = dir.join("update.asset");
    let checksum = dir.join("update.sha256");
    std::fs::write(&asset, b"abc").unwrap();
    for sidecar in [
        String::new(),
        " \t\n".to_owned(),
        "a".repeat(63),
        "a".repeat(65),
        "g".repeat(64),
        "é".repeat(32),
    ] {
        std::fs::write(&checksum, sidecar).unwrap();
        assert_eq!(
            super::verify_checksum(&asset, &checksum),
            Err("checksum sidecar malformed".to_owned())
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

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
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(unix)]
fn output_text(output: &std::process::Output) -> String {
    format!(
        "status {:?}\nstdout {}\nstderr {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(unix)]
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
    #[cfg(not(windows))]
    {
        let staged = std::path::Path::new("/tmp/next");
        let current = std::path::Path::new("/tmp/app");
        let quiet = super::unix_script(staged, current, "ab", false);
        assert!(!quiet.contains("nohup"), "{quiet}");
        let live = super::unix_script(staged, current, "ab", true);
        assert!(live.contains("nohup \"$current\""), "{live}");
    }
    let staged = std::path::Path::new("/tmp/next");
    let current = std::path::Path::new("/tmp/app");
    let quiet = super::apply::helper_arguments(current, staged, "ab", 1, false);
    assert!(!quiet.iter().any(|arg| arg == "--relaunch"));
    let live = super::apply::helper_arguments(current, staged, "ab", 1, true);
    assert!(live.iter().any(|arg| arg == "--relaunch"));
    assert!(live.windows(2).any(|pair| pair == ["--target", "/tmp/app"]));
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
fn unix_script_good_hash_replaces_and_removes_the_backup() {
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
    assert!(
        !backup.exists(),
        "a verified replace must not leave the old-version backup behind"
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
    assert!(
        !backup.exists(),
        "old-version backup must be removed after the verified swap"
    );
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

fn release_assets() -> Vec<super::AssetJson> {
    let names = [
        "mycode-desktop-v0.9.18-aarch64-apple-darwin.zip",
        "mycode-desktop-v0.9.18-aarch64-apple-darwin.zip.sha256",
        "mycode-desktop-v0.9.18-aarch64-pc-windows-msvc.zip",
        "mycode-desktop-v0.9.18-aarch64-pc-windows-msvc.zip.sha256",
        "mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip",
        "mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip.sha256",
    ];
    names
        .into_iter()
        .map(|name| super::AssetJson {
            name: name.to_owned(),
            browser_download_url: format!(
                "https://github.com/MCapricorns/mycode/releases/download/v0.9.18/{name}"
            ),
        })
        .collect()
}

#[test]
fn asset_prefix_names_each_binary_family() {
    assert_eq!(super::asset_prefix("mycode-desktop"), "mycode-desktop-v");
    assert_eq!(super::asset_prefix("mycode"), "mycode-v");
}

#[test]
fn a_renamed_binary_keeps_the_desktop_family() {
    assert_eq!(super::asset_family(Some("mycode")), "mycode");
    assert_eq!(super::asset_family(Some("MYCode")), "mycode");
    assert_eq!(
        super::asset_family(Some("mycode-desktop")),
        "mycode-desktop"
    );
    assert_eq!(
        super::asset_family(Some("MYCode Desktop")),
        "mycode-desktop"
    );
    assert_eq!(super::asset_family(None), "mycode-desktop");
}

#[test]
fn windows_x86_64_selects_the_published_zip_not_the_checksum() {
    let offer = super::resolve_asset_for(
        "windows",
        "x86_64",
        "mycode-desktop",
        "v0.9.18",
        "https://github.com/MCapricorns/mycode/releases/tag/v0.9.18",
        &release_assets(),
    )
    .unwrap()
    .expect("windows x86_64 is a published update target");
    assert_eq!(offer.version, "0.9.18");
    assert!(
        offer
            .asset_url
            .ends_with("mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip")
    );
    assert!(
        offer
            .checksum_url
            .ends_with("mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip.sha256")
    );
    assert!(!offer.asset_url.ends_with(".sha256"));
}

#[test]
fn an_asset_hosted_outside_the_release_is_refused() {
    for url in [
        "https://evil.example/mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip",
        "https://github.com/someone/else/releases/download/v0.9.18/mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip",
        "https://github.com/MCapricorns/mycode/releases/download/../../x/mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip",
        "https://github.com/MCapricorns/mycode/releases/download/v0.9.18/mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip?x=1",
    ] {
        let assets = vec![super::AssetJson {
            name: "mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip".to_owned(),
            browser_download_url: url.to_owned(),
        }];
        let result = super::resolve_asset_for(
            "windows",
            "x86_64",
            "mycode-desktop",
            "v0.9.18",
            "https://github.com/MCapricorns/mycode/releases/tag/v0.9.18",
            &assets,
        );
        assert!(result.is_err(), "{url}");
    }
}

#[test]
fn the_cli_stem_never_matches_desktop_assets() {
    // `mycode` is a strict prefix of `mycode-desktop`: the `-v` boundary
    // keeps the CLI from installing the desktop archive.
    let result = super::resolve_asset_for(
        "windows",
        "x86_64",
        "mycode",
        "v0.9.18",
        "https://github.com/MCapricorns/mycode/releases/tag/v0.9.18",
        &release_assets(),
    );
    assert!(
        result.is_err(),
        "a `mycode` binary must not match `mycode-desktop-*` assets"
    );
}

#[test]
fn the_cli_stem_selects_the_cli_zip() {
    let mut assets = release_assets();
    assets.push(super::AssetJson {
        name: "mycode-v0.9.18-x86_64-pc-windows-msvc.zip".to_owned(),
        browser_download_url:
            "https://github.com/MCapricorns/mycode/releases/download/v0.9.18/mycode-v0.9.18-x86_64-pc-windows-msvc.zip"
                .to_owned(),
    });
    let offer = super::resolve_asset_for(
        "windows",
        "x86_64",
        "mycode",
        "v0.9.18",
        "https://github.com/MCapricorns/mycode/releases/tag/v0.9.18",
        &assets,
    )
    .unwrap()
    .expect("windows x86_64 is a published update target");
    assert!(
        offer
            .asset_url
            .ends_with("mycode-v0.9.18-x86_64-pc-windows-msvc.zip")
    );
    assert!(
        offer
            .checksum_url
            .ends_with("mycode-v0.9.18-x86_64-pc-windows-msvc.zip.sha256")
    );
}

#[test]
fn an_archive_entry_with_a_drive_prefix_is_refused() {
    use std::io::Write as _;

    let dir = std::env::temp_dir().join(format!("mycode-zip-prefix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["C:evil.exe", "../evil.exe", "/abs/mycode-desktop"] {
        let asset = dir.join("asset.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&asset).unwrap());
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"payload").unwrap();
        writer.finish().unwrap();
        let error = super::extract_binary(&asset, &dir, "mycode-desktop").expect_err(name);
        assert!(error.contains("unsafe"), "{name}: {error}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unsupported_platforms_have_no_asset() {
    assert_eq!(asset_suffix_for("macos", "x86_64"), "");
    assert_eq!(asset_suffix_for("linux", "x86_64"), "");
    assert_eq!(asset_suffix_for("linux", "aarch64"), "");
    assert_eq!(asset_suffix_for("windows", "x86"), "");
    assert_eq!(asset_suffix_for("freebsd", "x86_64"), "");
}
