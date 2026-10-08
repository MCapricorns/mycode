//! Default outbound User-Agent construction.
//!
//! OS and platform detection lives here rather than in the strict settings
//! schema module: it probes the environment instead of describing document
//! grammar.

/// Builds the default outbound User-Agent, matching the pi agent identity
/// `pi (<platform> <release>; <arch>)` (pinned to pi-mono 0.85.1).
#[must_use]
pub fn default_user_agent() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("pi ({platform} {}; {arch})", os_release())
}

fn os_release() -> String {
    #[cfg(unix)]
    {
        rustix::system::uname()
            .release()
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(windows)]
    {
        windows_release()
    }
    #[cfg(not(any(unix, windows)))]
    {
        "unknown".to_owned()
    }
}

#[cfg(windows)]
fn windows_release() -> String {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        dwMajorVersion: 0,
        dwMinorVersion: 0,
        dwBuildNumber: 0,
        dwPlatformId: 0,
        szCSDVersion: [0; 128],
    };
    // SAFETY: `info` is a valid OSVERSIONINFOW with the correct size set;
    // RtlGetVersion only writes into it and reports success via NTSTATUS.
    let status = unsafe { RtlGetVersion(&mut info) };
    if status == 0 {
        format!(
            "{}.{}.{}",
            info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        )
    } else {
        "unknown".to_owned()
    }
}
