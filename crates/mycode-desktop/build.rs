//! Embeds the application icon as Windows resource id 1, which gpui's
//! window class loads for the title bar, taskbar, and alt-tab entries.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(target_os = "windows")]
    embed_icon();
}

/// The icon resource only exists on Windows; the winresource dependency is
/// declared under `[target.'cfg(windows)'.build-dependencies]`, so the
/// embedding call must be windows-only too.
#[cfg(target_os = "windows")]
fn embed_icon() {
    if std::path::Path::new("assets/icon.ico").exists() {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource
            .compile()
            .expect("embed the application icon resource");
    }
}
