//! Kernel-loadable image classification for structured exec.
//!
//! Only PE, ELF, and Mach-O / fat Mach-O images are accepted. Shebang
//! scripts, batch files, and any implicit interpreter fallback are rejected
//! here; the platform launch path never calls `cmd.exe` or `ShellExecute`.
use crate::tool::ToolError;

/// Kind of kernel-loadable image proven from magic bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImageKind {
    /// ELF `ET_EXEC` or `ET_DYN` (PIE).
    Elf,
    /// PE32/PE32+ image (`MZ` + `PE\0\0`).
    Pe,
    /// Thin or fat Mach-O.
    MachO {
        /// True when the file is a fat / universal binary.
        fat: bool,
    },
}

/// Maximum `e_lfanew` accepted when locating the PE signature.
const MAX_PE_LFANEW: usize = 64 * 1024;
/// ELF `EI_DATA` value for little-endian fields.
const ELF_DATA_LITTLE_ENDIAN: u8 = 1;
/// ELF `EI_DATA` value for big-endian fields.
const ELF_DATA_BIG_ENDIAN: u8 = 2;
/// ELF `EI_CLASS` value for 32-bit objects.
const ELF_CLASS_32: u8 = 1;
/// ELF `EI_CLASS` value for 64-bit objects.
const ELF_CLASS_64: u8 = 2;
/// ELF `e_machine` for i386.
const EM_386: u16 = 3;
/// ELF `e_machine` for x86-64.
const EM_X86_64: u16 = 62;
/// ELF `e_machine` for ARM.
const EM_ARM: u16 = 40;
/// ELF `e_machine` for AArch64.
const EM_AARCH64: u16 = 183;
/// ELF `e_machine` for RISC-V.
const EM_RISCV: u16 = 243;

/// Classifies a kernel-loadable image from a prefix of the file.
///
/// `header` should contain at least the first 4 KiB when available. When the
/// PE offset lies beyond `header`, `pe_tail` must contain the four signature
/// bytes at that offset.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when the bytes are a shebang, a batch
/// script, a DOS MZ stub without PE, or otherwise not a supported image.
pub(super) fn classify_image(
    header: &[u8],
    pe_tail: Option<&[u8; 4]>,
) -> Result<ImageKind, ToolError> {
    if header.starts_with(b"#!") {
        return Err(ToolError::InvalidArgs(
            "program is a shebang script; shell program mode runs kernel-loadable \
             images only (pass an explicit interpreter, or use shell mode script)"
                .into(),
        ));
    }
    if looks_like_batch(header) {
        return Err(ToolError::InvalidArgs(
            "program is a batch script; shell program mode never invokes cmd.exe".into(),
        ));
    }
    if header.starts_with(b"\x7fELF") {
        return classify_elf(header);
    }
    if header.len() >= 4 {
        let magic =
            u32::from_le_bytes(header[..4].try_into().expect("header length is at least 4"));
        let magic_be =
            u32::from_be_bytes(header[..4].try_into().expect("header length is at least 4"));
        // MH_MAGIC_64 / MH_MAGIC / FAT_MAGIC / FAT_MAGIC_64 from <mach-o/loader.h>
        // and <mach-o/fat.h>. Opposite-endian CIGAM values are rejected.
        match (magic, magic_be) {
            (0xfeed_facf | 0xfeed_face, _) => return Ok(ImageKind::MachO { fat: false }),
            (_, 0xcafe_babe | 0xcafe_babf) => return Ok(ImageKind::MachO { fat: true }),
            (0xcffa_edfe | 0xcefa_edfe, _) | (_, 0xbeba_feca | 0xbfba_feca) => {
                return Err(ToolError::InvalidArgs(
                    "program is opposite-endian Mach-O, which the kernel rejects".into(),
                ));
            }
            _ => {}
        }
    }
    if header.starts_with(b"MZ") {
        return classify_pe(header, pe_tail);
    }
    Err(ToolError::InvalidArgs(
        "program is not a kernel-loadable PE, ELF, or Mach-O image".into(),
    ))
}

fn classify_elf(header: &[u8]) -> Result<ImageKind, ToolError> {
    if header.len() < 20 {
        return Err(ToolError::InvalidArgs(
            "program ELF header is truncated".into(),
        ));
    }
    let ei_class = header[4];
    if ei_class != ELF_CLASS_32 && ei_class != ELF_CLASS_64 {
        return Err(ToolError::InvalidArgs(
            "program ELF class is not 32-bit or 64-bit".into(),
        ));
    }
    let byte_order = header[5];
    let native_byte_order = if cfg!(target_endian = "little") {
        ELF_DATA_LITTLE_ENDIAN
    } else {
        ELF_DATA_BIG_ENDIAN
    };
    if byte_order != native_byte_order {
        let message = match byte_order {
            ELF_DATA_LITTLE_ENDIAN | ELF_DATA_BIG_ENDIAN => {
                "program ELF byte order is incompatible with this target"
            }
            _ => "program ELF byte order is invalid",
        };
        return Err(ToolError::InvalidArgs(message.into()));
    }
    let encoded_type = [header[16], header[17]];
    let encoded_machine = [header[18], header[19]];
    let (image_type, machine) = match byte_order {
        ELF_DATA_LITTLE_ENDIAN => (
            u16::from_le_bytes(encoded_type),
            u16::from_le_bytes(encoded_machine),
        ),
        ELF_DATA_BIG_ENDIAN => (
            u16::from_be_bytes(encoded_type),
            u16::from_be_bytes(encoded_machine),
        ),
        _ => unreachable!("ELF byte order was validated"),
    };
    // ET_EXEC=2 and ET_DYN=3. ET_DYN covers position-independent executables.
    if image_type != 2 && image_type != 3 {
        return Err(ToolError::InvalidArgs(
            "program ELF type is not an executable or PIE image".into(),
        ));
    }
    let Some((expected_machine, expected_class)) = supported_elf_machine_and_class() else {
        return Err(ToolError::InvalidArgs(
            "program ELF is not supported on this target".into(),
        ));
    };
    if machine != expected_machine || ei_class != expected_class {
        return Err(ToolError::InvalidArgs(
            "program ELF machine or class is incompatible with this target".into(),
        ));
    }
    Ok(ImageKind::Elf)
}

/// Returns the ELF `e_machine` and `EI_CLASS` this compiled target can load.
const fn supported_elf_machine_and_class() -> Option<(u16, u8)> {
    if cfg!(target_arch = "x86_64") {
        Some((EM_X86_64, ELF_CLASS_64))
    } else if cfg!(target_arch = "x86") {
        Some((EM_386, ELF_CLASS_32))
    } else if cfg!(target_arch = "aarch64") {
        Some((EM_AARCH64, ELF_CLASS_64))
    } else if cfg!(target_arch = "arm") {
        Some((EM_ARM, ELF_CLASS_32))
    } else if cfg!(target_arch = "riscv64") {
        Some((EM_RISCV, ELF_CLASS_64))
    } else {
        None
    }
}

fn classify_pe(header: &[u8], pe_tail: Option<&[u8; 4]>) -> Result<ImageKind, ToolError> {
    if header.len() < 0x40 {
        return Err(ToolError::InvalidArgs(
            "program MZ header is truncated".into(),
        ));
    }
    let lfanew =
        u32::from_le_bytes(header[0x3C..0x40].try_into().expect("slice length is 4")) as usize;
    if !(0x40..=MAX_PE_LFANEW).contains(&lfanew) {
        return Err(ToolError::InvalidArgs(
            "program MZ header has an invalid PE offset".into(),
        ));
    }
    let signature = if lfanew + 4 <= header.len() {
        &header[lfanew..lfanew + 4]
    } else {
        pe_tail.ok_or_else(|| ToolError::InvalidArgs("program PE signature is missing".into()))?
    };
    if signature != b"PE\0\0" {
        return Err(ToolError::InvalidArgs(
            "program is a DOS MZ image without a PE signature".into(),
        ));
    }
    Ok(ImageKind::Pe)
}

fn looks_like_batch(header: &[u8]) -> bool {
    let prefix = std::str::from_utf8(header.get(..64).unwrap_or(header))
        .unwrap_or("")
        .trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    let lower = prefix.to_ascii_lowercase();
    lower.starts_with("@echo") || lower.starts_with("rem ") || lower.starts_with(": ")
}

/// Reads the four PE signature bytes at `lfanew` when they lie past `header`.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when the offset is unreadable.
pub(super) fn read_pe_tail(
    file: &mut std::fs::File,
    header: &[u8],
) -> Result<Option<[u8; 4]>, ToolError> {
    if !header.starts_with(b"MZ") || header.len() < 0x40 {
        return Ok(None);
    }
    let lfanew =
        u32::from_le_bytes(header[0x3C..0x40].try_into().expect("slice length is 4")) as usize;
    if lfanew + 4 <= header.len() {
        return Ok(None);
    }
    if !(0x40..=MAX_PE_LFANEW).contains(&lfanew) {
        return Ok(None);
    }
    use std::io::{Read as _, Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(lfanew as u64)).map_err(|err| {
        ToolError::InvalidArgs(format!("program PE signature is unreadable: {err}"))
    })?;
    let mut tail = [0_u8; 4];
    file.read_exact(&mut tail).map_err(|err| {
        ToolError::InvalidArgs(format!("program PE signature is unreadable: {err}"))
    })?;
    file.seek(SeekFrom::Start(0))
        .map_err(|err| ToolError::InvalidArgs(format!("program could not be rewound: {err}")))?;
    Ok(Some(tail))
}
