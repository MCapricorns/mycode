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
        crate::core::unix_now().as_nanos()
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
fn set_unix_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn run_sh(script: &std::path::Path) -> std::process::Output {
    std::process::Command::new("/bin/sh")
        .arg(script)
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn unix_script_good_hash_replaces_and_removes_the_backup() {
    let dir = scratch("unix-good-__CURRENT__-__NEW__-__HASH__-__RELAUNCH__");
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
fn windows_x86_64_selects_the_published_zip_not_the_checksum() {
    let offer = super::resolve_asset_for(
        "windows",
        "x86_64",
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
fn update_asset_must_match_the_release_version_and_download_path() {
    let mut assets = release_assets();
    assert!(super::resolve_asset_for("windows", "x86_64", "v0.9.19", "", &assets).is_err());
    for asset in &mut assets {
        asset.browser_download_url = asset
            .browser_download_url
            .replace("download/v0.9.18/", "download/v0.9.17/");
    }
    assert!(super::resolve_asset_for("windows", "x86_64", "v0.9.18", "", &assets).is_err());
}

#[test]
fn archive_helpers_are_not_installed_as_the_application() {
    use std::io::Write as _;
    let dir = scratch("archive-helpers");
    let asset = dir.join("asset.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&asset).unwrap());
    for (name, bytes) in [
        ("helper.exe", b"helper".as_slice()),
        ("mycode-desktop.exe", b"application".as_slice()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
    let binary = super::extract_binary(&asset, &dir).unwrap();
    assert_eq!(binary.file_name().unwrap(), "mycode-desktop.exe");
    assert_eq!(std::fs::read(binary).unwrap(), b"application");
    std::fs::remove_dir_all(dir).unwrap();
}
