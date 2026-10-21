//! The application's outbound HTTP identity.
//!
//! The User-Agent is fixed by the application — it is not a settings value:
//! `MYCode-Agent/<version> (<platform> <release>; <arch>; <shell>)`. The
//! shell is the one the shell tool resolved for this host, so a change in
//! shell configuration shows up in the string.

use std::sync::OnceLock;

/// The outbound User-Agent shared by provider, gateway-list, and update calls.
/// Detection runs once; the result cannot change during a session.
#[must_use]
pub fn default_user_agent() -> String {
    static AGENT: OnceLock<String> = OnceLock::new();
    AGENT
        .get_or_init(|| {
            format!(
                "MYCode-Agent/{} ({} {}; {}; {})",
                env!("CARGO_PKG_VERSION"),
                platform(),
                os_release(),
                arch(),
                shell()
            )
        })
        .clone()
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn shell() -> &'static str {
    mycode_tools::resolved_shell().kind.as_str()
}

fn os_release() -> String {
    #[cfg(unix)]
    {
        unix_release()
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

#[cfg(unix)]
fn unix_release() -> String {
    // SAFETY: `buf` is a valid `utsname` buffer; `uname` writes into it and
    // reports success through its return value.
    unsafe {
        let mut buf = std::mem::zeroed::<libc::utsname>();
        if libc::uname(&mut buf) == 0 {
            std::ffi::CStr::from_ptr(buf.release.as_ptr())
                .to_string_lossy()
                .into_owned()
        } else {
            "unknown".to_owned()
        }
    }
}

/// Real version via `RtlGetVersion` — `GetVersionEx` lies on unmanifested
/// processes.
#[cfg(windows)]
fn windows_release() -> String {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..OSVERSIONINFOW::default()
    };
    // SAFETY: `info` is a valid OSVERSIONINFOW with the correct size set;
    // `RtlGetVersion` writes into it and reports success through NTSTATUS.
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

#[cfg(test)]
mod tests {
    #[test]
    fn agent_names_product_version_platform_and_shell() {
        let agent = super::default_user_agent();
        assert!(
            agent.starts_with(&format!("MYCode-Agent/{} (", env!("CARGO_PKG_VERSION"))),
            "agent {agent}"
        );
        for shell in ["pwsh", "powershell", "bash", "zsh", "sh", "cmd"] {
            if agent.ends_with(&format!("{shell})")) {
                return;
            }
        }
        panic!("agent ends with a detected shell: {agent}");
    }
}
