//! Owner-only security descriptors for payload temps.
use std::io;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Owner-only protected DACL used for payload temps.
///
/// The descriptor is allocated by
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW` and freed with
/// `LocalFree`. It must stay alive for the `NtCreateFile` that consumes it.
pub(super) struct PrivateSd(pub(super) PSECURITY_DESCRIPTOR);

impl PrivateSd {
    pub(super) fn as_ptr(&self) -> *const SECURITY_DESCRIPTOR {
        self.0.cast()
    }
}

impl Drop for PrivateSd {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `ConvertStringSecurityDescriptorToSecurityDescriptorW`
            // allocated this descriptor.
            let _ = unsafe { LocalFree(self.0.cast()) };
        }
    }
}

/// Builds a protected DACL granting full access only to the current user
/// and SYSTEM, so a permissive parent cannot make the payload temp
/// world-readable.
pub(super) fn private_temp_descriptor() -> io::Result<PrivateSd> {
    let mut token = INVALID_HANDLE_VALUE;
    // SAFETY: `GetCurrentProcess` is a pseudo-handle that is not closed;
    // `token` is written only on success and is then an owned handle.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    struct TokenGuard(HANDLE);
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            if self.0 != INVALID_HANDLE_VALUE && !self.0.is_null() {
                // SAFETY: `OpenProcessToken` returned this owned handle.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }
    let token = TokenGuard(token);
    let mut needed = 0u32;
    // SAFETY: size probe; `needed` is written even when the call fails with
    // `ERROR_INSUFFICIENT_BUFFER`.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: `buffer` is writable storage of `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if (needed as usize) < size_of::<TOKEN_USER>() || (needed as usize) > buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token user information is truncated",
        ));
    }
    // SAFETY: `GetTokenInformation` filled a `TOKEN_USER` at `buffer`.
    let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read_unaligned() };
    let mut sid_text: windows_sys::core::PWSTR = null_mut();
    // SAFETY: `user.User.Sid` aliases `buffer`; `sid_text` is written on
    // success and owned by `LocalFree`.
    let ok = unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_text) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    struct SidText(*mut u16);
    impl Drop for SidText {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `ConvertSidToStringSidW` allocated this string.
                let _ = unsafe { LocalFree(self.0.cast()) };
            }
        }
    }
    let sid_text = SidText(sid_text);
    let mut sid_len = 0usize;
    // SAFETY: `sid_text` is a live NUL-terminated UTF-16 allocation.
    unsafe {
        while *sid_text.0.add(sid_len) != 0 {
            sid_len += 1;
        }
    }
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sid_text.0, sid_len) });
    // Protected DACL: current user and SYSTEM only. `P` blocks parent
    // inheritance so a shared directory cannot reopen the payload.
    let sddl = format!("D:P(A;;FA;;;{sid})(A;;FA;;;SY)");
    let mut wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wide` is a live NUL-terminated SDDL string; on success `sd`
    // is an allocation that `PrivateSd` frees.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_mut_ptr(),
            1, // SDDL_REVISION_1
            &mut sd,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PrivateSd(sd))
}
