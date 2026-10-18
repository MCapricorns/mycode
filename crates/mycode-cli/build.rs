//! Embeds the crab icon and the file-description/version block as Windows
//! resources so `mycode-cli.exe` properties show both. Other platforms read
//! the same strings from `--version`.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(target_os = "windows")]
    embed_resources();
}

/// The resource script only exists on Windows; the winresource dependency
/// is declared under `[target.'cfg(windows)'.build-dependencies]`, so the
/// embedding call must be windows-only too.
#[cfg(target_os = "windows")]
fn embed_resources() {
    let mut resource = winresource::WindowsResource::new();
    let icon = "assets/icon.ico";
    if std::path::Path::new(icon).exists() {
        resource.set_icon(icon);
    }
    resource
        .set("FileDescription", "MYCode CLI")
        .set("ProductName", "MYCode")
        .set("InternalName", "mycode-cli")
        .set("OriginalFilename", "mycode-cli.exe")
        .set("LegalCopyright", "Apache-2.0");
    if let Ok(version) = std::env::var("CARGO_PKG_VERSION") {
        let parts: Vec<u64> = version
            .split('.')
            .filter_map(|part| part.parse().ok())
            .collect();
        if parts.len() >= 3 {
            let packed = (parts[0] << 48)
                | (parts[1] << 32)
                | (parts[2] << 16)
                | parts.get(3).copied().unwrap_or(0);
            resource
                .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed)
                .set_version_info(winresource::VersionInfo::FILEVERSION, packed);
        }
    }
    resource.compile().expect("embed mycode-cli resources");
}
