//! Exact owner and protected-DACL handling for Windows owned directories.

use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
    GetSecurityDescriptorControl, GetTokenInformation, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    SECURITY_DESCRIPTOR_CONTROL, SetKernelObjectSecurity, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    ReOpenFile, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::{ConfigError, ConfigErrorKind};

const SDDL_REVISION_1: u32 = 1;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const SYSTEM_SID: &str = "S-1-5-18";

/// Owner and DACL facts extracted from one opened object.
#[derive(Debug)]
struct AclEvidence {
    owner_allowed: bool,
    owner_current_user: bool,
    owner_system: bool,
    current_user: bool,
    system: bool,
    protected: bool,
    ace_count: u32,
    extra_aces: u32,
}

pub(super) struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    pub(super) fn as_ptr(&self) -> *mut core::ffi::c_void {
        self.0.cast()
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: ConvertStringSecurityDescriptorToSecurityDescriptorW
            // allocates this descriptor with the LocalAlloc family.
            let _ = unsafe { LocalFree(self.0.cast()) };
        }
    }
}

struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: OpenProcessToken returned this owned handle.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

struct SidText(*mut u16);

impl Drop for SidText {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: ConvertSidToStringSidW allocated this LocalAlloc string.
            let _ = unsafe { LocalFree(self.0.cast()) };
        }
    }
}

pub(super) fn protected_descriptor() -> Result<SecurityDescriptor, ConfigError> {
    let current_sid = current_user_sid_string()?;
    descriptor_from_sddl(&protected_sddl(&current_sid))
}

fn protected_sddl(current_sid: &str) -> String {
    if current_sid == SYSTEM_SID {
        format!("O:{current_sid}D:P(A;;FA;;;{current_sid})")
    } else {
        format!("O:{current_sid}D:P(A;;FA;;;{current_sid})(A;;FA;;;SY)")
    }
}

fn descriptor_from_sddl(sddl: &str) -> Result<SecurityDescriptor, ConfigError> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wide` is a live NUL-terminated SDDL string and the output
    // pointer is writable.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(error.kind()));
    }
    if descriptor.is_null() {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(SecurityDescriptor(descriptor))
}

pub(super) fn require_current_owner(file: &File) -> Result<(), ConfigError> {
    let current_sid = current_user_sid_string()?;
    if inspect_handle_with_sid(file, &current_sid)?.owner_current_user {
        Ok(())
    } else {
        Err(ConfigError::new(ConfigErrorKind::AccessControl))
    }
}

pub(super) fn secure_existing_object(file: &File) -> Result<(), ConfigError> {
    let current_sid = current_user_sid_string()?;
    let existing = inspect_handle_with_sid(file, &current_sid)?;
    if is_exact_fixed_descriptor(&existing, &current_sid) {
        return Ok(());
    }
    if !existing.owner_allowed {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    let owner_current_user = existing.owner_current_user;

    let descriptor = protected_descriptor()?;
    let reopened;
    let target = if owner_current_user {
        file
    } else if existing.owner_system {
        reopened = reopen_for_owner_change(file)?;
        &reopened
    } else {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    };
    let security_information = DACL_SECURITY_INFORMATION
        | PROTECTED_DACL_SECURITY_INFORMATION
        | if owner_current_user {
            0
        } else {
            OWNER_SECURITY_INFORMATION
        };
    // `SetSecurityInfo` also walks children and rewrites inherited ACEs.
    // The session index creates and holds `sessions.db` in this directory
    // before the first settings read, so that walk can fail the call or
    // strip the database's access. This object's descriptor is the only
    // one the owned-path check enforces.
    // SAFETY: `target` is live with WRITE_DAC and, when replacing a SYSTEM
    // owner, WRITE_OWNER. `descriptor` stays live for this synchronous call.
    let updated = unsafe {
        SetKernelObjectSecurity(
            target.as_raw_handle(),
            security_information,
            descriptor.as_ptr(),
        )
    };
    if updated == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(error.kind()));
    }
    verify_fixed_descriptor(target)
}

fn reopen_for_owner_change(file: &File) -> Result<File, ConfigError> {
    // SAFETY: `file` is live. ReOpenFile returns a distinct owned handle or the
    // documented INVALID_HANDLE_VALUE sentinel.
    let handle = unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            READ_CONTROL | WRITE_DAC | WRITE_OWNER,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(error.kind()));
    }
    // SAFETY: ReOpenFile returned a fresh successful handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub(super) fn verify_fixed_descriptor(file: &File) -> Result<(), ConfigError> {
    let current_sid = current_user_sid_string()?;
    let evidence = inspect_handle_with_sid(file, &current_sid)?;
    if is_exact_fixed_descriptor(&evidence, &current_sid) {
        Ok(())
    } else {
        Err(ConfigError::new(ConfigErrorKind::AccessControl))
    }
}

fn is_exact_fixed_descriptor(evidence: &AclEvidence, current_sid: &str) -> bool {
    evidence.owner_allowed
        && evidence.owner_current_user
        && evidence.current_user
        && evidence.system
        && evidence.protected
        && evidence.extra_aces == 0
        && evidence.ace_count == expected_ace_count(current_sid)
}

fn inspect_handle_with_sid(file: &File, current_sid: &str) -> Result<AclEvidence, ConfigError> {
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut raw_descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `file` is live. On success `raw_descriptor` is LocalFree-owned.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut raw_descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        let error = io::Error::from_raw_os_error(status as i32);
        return Err(ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(error.kind()));
    }
    if raw_descriptor.is_null() {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    let descriptor = SecurityDescriptor(raw_descriptor);
    inspect_descriptor(&descriptor, owner, dacl, current_sid)
}

fn inspect_descriptor(
    descriptor: &SecurityDescriptor,
    owner: *mut core::ffi::c_void,
    dacl: *mut ACL,
    current_sid: &str,
) -> Result<AclEvidence, ConfigError> {
    let mut control: SECURITY_DESCRIPTOR_CONTROL = 0;
    let mut revision = 0u32;
    // SAFETY: `descriptor` owns a live valid security descriptor.
    let queried =
        unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) };
    if queried == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(error.kind()));
    }
    let owner_text = sid_string(owner);
    let owner_current_user = owner_text.as_deref() == Some(current_sid);
    let owner_system = owner_text.as_deref() == Some(SYSTEM_SID);
    let ace_evidence = inspect_aces(dacl, current_sid);
    Ok(AclEvidence {
        owner_allowed: owner_current_user || owner_system,
        owner_current_user,
        owner_system,
        current_user: ace_evidence.current_user,
        system: ace_evidence.system,
        protected: control & SE_DACL_PROTECTED != 0,
        ace_count: ace_evidence.ace_count,
        extra_aces: ace_evidence.extra_aces,
    })
}

struct AceEvidence {
    current_user: bool,
    system: bool,
    ace_count: u32,
    extra_aces: u32,
}

fn inspect_aces(dacl: *mut ACL, current_sid: &str) -> AceEvidence {
    if dacl.is_null() {
        return AceEvidence {
            current_user: false,
            system: false,
            ace_count: 0,
            extra_aces: 1,
        };
    }
    // SAFETY: `dacl` aliases a live security descriptor.
    let ace_count = unsafe { u32::from((*dacl).AceCount) };
    let mut current_user = false;
    let mut system = false;
    let mut extra_aces = 0u32;
    for index in 0..ace_count {
        let mut raw_ace: *mut core::ffi::c_void = null_mut();
        // SAFETY: GetAce validates the index and writes `raw_ace` on success.
        let queried = unsafe { GetAce(dacl, index, &mut raw_ace) };
        if queried == 0 || raw_ace.is_null() {
            extra_aces = extra_aces.saturating_add(1);
            continue;
        }
        // SAFETY: GetAce returned an ACE pointer in the live DACL.
        let header = unsafe { raw_ace.cast::<ACE_HEADER>().read() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0 {
            extra_aces = extra_aces.saturating_add(1);
            continue;
        }
        // SAFETY: ACCESS_ALLOWED_ACE is the layout selected by AceType.
        let allowed = unsafe { raw_ace.cast::<ACCESS_ALLOWED_ACE>().read() };
        if allowed.Mask != FILE_ALL_ACCESS {
            extra_aces = extra_aces.saturating_add(1);
            continue;
        }
        // SAFETY: The SID begins at SidStart within ACCESS_ALLOWED_ACE.
        let sid_pointer = unsafe {
            std::ptr::addr_of!((*raw_ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart)
                .cast::<u8>()
                .cast_mut()
                .cast()
        };
        let Some(sid) = sid_string(sid_pointer) else {
            extra_aces = extra_aces.saturating_add(1);
            continue;
        };
        if sid == current_sid {
            if current_user {
                extra_aces = extra_aces.saturating_add(1);
            }
            current_user = true;
            if current_sid == SYSTEM_SID {
                system = true;
            }
        } else if sid == SYSTEM_SID {
            if system {
                extra_aces = extra_aces.saturating_add(1);
            }
            system = true;
        } else {
            extra_aces = extra_aces.saturating_add(1);
        }
    }
    AceEvidence {
        current_user,
        system,
        ace_count,
        extra_aces,
    }
}

pub(super) fn current_user_sid_string() -> Result<String, ConfigError> {
    let mut raw_token = INVALID_HANDLE_VALUE;
    // SAFETY: GetCurrentProcess returns a borrowed pseudo-handle. The output
    // token is written only on success.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) };
    if opened == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(error.kind()));
    }
    if raw_token.is_null() || raw_token == INVALID_HANDLE_VALUE {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    let token = TokenHandle(raw_token);
    let mut required = 0u32;
    // SAFETY: This is the documented size query with a null buffer.
    let queried = unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut required) };
    if queried != 0 {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        || usize::try_from(required).unwrap_or(0) < size_of::<TOKEN_USER>()
    {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(error.kind()));
    }
    let mut buffer = vec![0u8; required as usize];
    // SAFETY: `buffer` is writable storage of exactly `required` bytes.
    let queried = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    };
    if queried == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(error.kind()));
    }
    if (required as usize) < size_of::<TOKEN_USER>() || (required as usize) > buffer.len() {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    // SAFETY: GetTokenInformation initialized TOKEN_USER at the buffer start.
    let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read_unaligned() };
    sid_string(user.User.Sid).ok_or_else(|| ConfigError::new(ConfigErrorKind::AccessControl))
}

fn sid_string(sid: *mut core::ffi::c_void) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut raw_text: windows_sys::core::PWSTR = null_mut();
    // SAFETY: Callers provide a SID owned by a live token or descriptor.
    let converted = unsafe { ConvertSidToStringSidW(sid, &mut raw_text) };
    if converted == 0 || raw_text.is_null() {
        return None;
    }
    let text = SidText(raw_text);
    let mut length = 0usize;
    // SAFETY: ConvertSidToStringSidW returns a NUL-terminated UTF-16 string.
    unsafe {
        while *text.0.add(length) != 0 {
            length = length.saturating_add(1);
        }
        String::from_utf16(std::slice::from_raw_parts(text.0, length)).ok()
    }
}

fn expected_ace_count(current_sid: &str) -> u32 {
    if current_sid == SYSTEM_SID { 1 } else { 2 }
}
