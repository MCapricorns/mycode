//! Clipboard pictures for the composer.
//!
//! `Ctrl+V` attaches the picture the clipboard holds instead of typing it, and
//! a bracketed paste whose text is one image path attaches that file. Windows
//! reads the native clipboard: a registered `PNG` format when the source
//! application publishes one, otherwise the device-independent bitmap that
//! every screenshot tool writes, re-encoded as PNG. macOS asks `osascript`
//! for the clipboard's PNG flavour and converts a TIFF-only picture with
//! `sips`. Any failure is a plain message, never a panic: a terminal whose
//! clipboard holds text just stays text.

use std::path::PathBuf;

use mycode_core::MAX_IMAGE_BYTES;

/// One picture read out of the clipboard or a pasted path.
pub(super) struct ClipboardImage {
    /// Encoded bytes in the format `mime` names.
    pub(super) bytes: Vec<u8>,
    /// One of [`mycode_core::IMAGE_MEDIA_TYPES`].
    pub(super) mime: &'static str,
}

/// Reads the clipboard picture, if it holds one.
///
/// # Errors
///
/// Returns a message when the clipboard cannot be read at all; a clipboard
/// that simply holds no picture is `Ok(None)`.
pub(super) fn read() -> Result<Option<ClipboardImage>, String> {
    platform::read()
}

/// Reads one image file referenced by pasted text, if `text` names one.
///
/// Quoted paths (`"C:\Shots\a b.png"`) and `file://` URLs are accepted; any
/// other text is left to the composer. Oversized files are refused so a paste
/// cannot block the UI on a huge read.
pub(super) fn image_at(text: &str) -> Result<Option<ClipboardImage>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) {
        return Ok(None);
    }
    let path = local_path(trimmed);
    if !path.is_file() {
        return Ok(None);
    }
    let length = std::fs::metadata(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    if length > MAX_IMAGE_BYTES as u64 {
        return Err(format!("{}: image is larger than 3.75 MB", path.display()));
    }
    let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(mycode_core::image_media_type(&bytes).map(|mime| ClipboardImage { bytes, mime }))
}

/// The local path a pasted path or `file://` URL names.
pub(super) fn local_path(text: &str) -> PathBuf {
    let trimmed = text.trim().trim_matches(['"', '\'']);
    match trimmed.strip_prefix("file://") {
        Some(rest) => {
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            PathBuf::from(file_url_path(rest))
        }
        None => PathBuf::from(trimmed),
    }
}

/// A Windows drive URL spells the path `/C:/x`; the leading slash is not part
/// of the file name. Other platforms keep the URL path as it stands.
fn file_url_path(rest: &str) -> String {
    #[cfg(windows)]
    {
        let bytes = rest.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            return rest[1..].to_owned();
        }
    }
    rest.to_owned()
}

#[cfg(windows)]
mod platform {
    use std::io::Cursor;

    use windows_sys::Win32::Foundation::HGLOBAL;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    use super::ClipboardImage;

    /// `CF_DIB`: a `BITMAPINFOHEADER` followed by the pixels.
    const CF_DIB: u32 = 8;
    /// `CF_DIBV5`: a `BITMAPV5HEADER`, which can carry an alpha mask.
    const CF_DIBV5: u32 = 17;

    pub(super) fn read() -> Result<Option<ClipboardImage>, String> {
        // SAFETY: the clipboard is opened for the length of this call and the
        // handle is closed on every path after it succeeds. A null owner is
        // allowed for a process that is not a window.
        if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
            return Err(format!(
                "clipboard unavailable: {}",
                std::io::Error::last_os_error()
            ));
        }
        let result = read_formats();
        // SAFETY: closes the handle opened above, on success and on failure.
        unsafe {
            CloseClipboard();
        }
        result
    }

    fn read_formats() -> Result<Option<ClipboardImage>, String> {
        // An application that publishes PNG (browsers, Snipaste, ShareX) needs
        // no conversion, and its alpha is exact.
        let png = register_format("PNG");
        if png != 0
            && unsafe { IsClipboardFormatAvailable(png) } != 0
            && let Some(bytes) = read_bytes(png)
            && mycode_core::image_media_type(&bytes) == Some("image/png")
        {
            return Ok(Some(ClipboardImage {
                bytes,
                mime: "image/png",
            }));
        }
        for format in [CF_DIBV5, CF_DIB] {
            if unsafe { IsClipboardFormatAvailable(format) } == 0 {
                continue;
            }
            let Some(dib) = read_bytes(format) else {
                continue;
            };
            if let Some(bytes) = dib_to_png(&dib) {
                return Ok(Some(ClipboardImage {
                    bytes,
                    mime: "image/png",
                }));
            }
        }
        Ok(None)
    }

    fn register_format(name: &str) -> u32 {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is NUL-terminated and lives for the call.
        unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
    }

    /// Copies one clipboard format out of its global memory block.
    fn read_bytes(format: u32) -> Option<Vec<u8>> {
        // SAFETY: the clipboard is open. `GetClipboardData` returns either a
        // null handle or one owned by the clipboard, which is locked, copied,
        // and unlocked again before the clipboard is closed.
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let handle: HGLOBAL = handle;
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                return None;
            }
            let size = GlobalSize(handle);
            let bytes = if size == 0 {
                None
            } else {
                Some(std::slice::from_raw_parts(pointer.cast::<u8>(), size).to_vec())
            };
            GlobalUnlock(handle);
            bytes
        }
    }

    /// Re-encodes a device-independent bitmap as PNG.
    ///
    /// The decoder reads the DIB header itself, so no file header is needed;
    /// a screenshot's 32-bit `BI_RGB` pixels come back opaque, while a
    /// `BITMAPV5HEADER` alpha mask is kept.
    fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
        use image::ImageFormat;
        use image::codecs::bmp::BmpDecoder;

        let decoder = BmpDecoder::new_without_file_header(Cursor::new(dib)).ok()?;
        let picture = image::DynamicImage::from_decoder(decoder).ok()?;
        let mut png = Vec::new();
        picture
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .ok()?;
        Some(png)
    }

    #[cfg(test)]
    mod tests {
        use super::dib_to_png;

        /// A 2×2 32-bit `BI_RGB` DIB, the shape a screenshot tool puts on the
        /// clipboard: a 40-byte header, then bottom-up BGRA rows.
        fn tiny_dib() -> Vec<u8> {
            let header_size = 40u32;
            let width = 2i32;
            let height = 2i32;
            let mut dib = Vec::new();
            dib.extend_from_slice(&header_size.to_le_bytes());
            dib.extend_from_slice(&width.to_le_bytes());
            dib.extend_from_slice(&height.to_le_bytes());
            dib.extend_from_slice(&1u16.to_le_bytes()); // planes
            dib.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
            dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
            dib.extend_from_slice(&16u32.to_le_bytes()); // image size
            dib.extend_from_slice(&0i32.to_le_bytes()); // x pixels per meter
            dib.extend_from_slice(&0i32.to_le_bytes()); // y pixels per meter
            dib.extend_from_slice(&0u32.to_le_bytes()); // colors used
            dib.extend_from_slice(&0u32.to_le_bytes()); // important colors
            // Bottom row first: opaque red, opaque green.
            for color in [[0, 0, 255, 0], [0, 255, 0, 0]] {
                dib.extend_from_slice(&color);
            }
            // Top row: opaque blue, white.
            for color in [[255, 0, 0, 0], [255, 255, 255, 0]] {
                dib.extend_from_slice(&color);
            }
            dib
        }

        #[test]
        fn a_dib_is_reencoded_as_png() {
            let png = dib_to_png(&tiny_dib()).expect("png");
            assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
            let picture =
                image::load_from_memory_with_format(&png, image::ImageFormat::Png).expect("decode");
            assert_eq!(picture.width(), 2);
            assert_eq!(picture.height(), 2);
            let pixel = picture.to_rgba8().get_pixel(0, 1).0;
            assert_eq!(pixel, [255, 0, 0, 255], "bottom-left row is red");
        }

        #[test]
        fn a_truncated_dib_is_not_a_picture() {
            assert!(dib_to_png(&[0u8; 12]).is_none());
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;
    use std::process::Command;

    use super::ClipboardImage;

    pub(super) fn read() -> Result<Option<ClipboardImage>, String> {
        if let Some(path) = copy_flavour("PNGf", "png")? {
            let bytes =
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let _ = std::fs::remove_file(&path);
            if mycode_core::image_media_type(&bytes) == Some("image/png") {
                return Ok(Some(ClipboardImage {
                    bytes,
                    mime: "image/png",
                }));
            }
        }
        // Screenshots publish PNG, but Preview and Safari copy TIFF only;
        // `sips` turns that into the PNG the composer accepts.
        if let Some(bytes) = tiff_as_png() {
            return Ok(Some(ClipboardImage {
                bytes,
                mime: "image/png",
            }));
        }
        Ok(None)
    }

    /// Writes one pasteboard flavour to a temp file. `Ok(None)` means the
    /// pasteboard does not carry that flavour.
    fn copy_flavour(class: &str, suffix: &str) -> Result<Option<PathBuf>, String> {
        let path = temp_path(suffix);
        let script = format!(
            "set theData to the clipboard as «class {class}»\n\
             set theFile to open for access POSIX file \"{path}\" with write permission\n\
             set eof theFile to 0\n\
             write theData to theFile\n\
             close access theFile",
            path = path.display()
        );
        let status = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .status()
            .map_err(|error| format!("clipboard unavailable: {error}"))?;
        if !status.success() {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
        Ok(Some(path))
    }

    fn tiff_as_png() -> Option<Vec<u8>> {
        let tiff = copy_flavour("TIFF", "tiff").ok().flatten()?;
        let png = temp_path("png");
        let status = Command::new("sips")
            .args(["-s", "format", "png"])
            .arg(&tiff)
            .arg("--out")
            .arg(&png)
            .status()
            .ok()?;
        let bytes = status.success().then(|| std::fs::read(&png).ok()).flatten();
        let _ = std::fs::remove_file(&tiff);
        let _ = std::fs::remove_file(&png);
        bytes.filter(|bytes| mycode_core::image_media_type(bytes) == Some("image/png"))
    }

    fn temp_path(suffix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mycode-clipboard-{}-{}.{suffix}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.subsec_nanos())
                .unwrap_or_default()
        ))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::ClipboardImage;

    pub(super) fn read() -> Result<Option<ClipboardImage>, String> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::image_at;

    /// The leading bytes are all the path reader checks; the picture itself
    /// is never decoded.
    fn write_png(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mycode-clipboard-{label}-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.subsec_nanos())
                .unwrap_or_default()
        ));
        std::fs::write(
            &path,
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0],
        )
        .expect("png");
        path
    }

    #[test]
    fn a_pasted_image_path_is_recognized() {
        let path = write_png("paste");
        let text = path.display().to_string();
        let image = image_at(&text).expect("read").expect("image");
        assert_eq!(image.mime, "image/png");
        // Terminals quote a path with spaces; the quotes are not the name.
        let quoted = format!("\"{text}\"");
        assert!(image_at(&quoted).expect("read").is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ordinary_text_stays_text() {
        assert!(image_at("hello world").expect("read").is_none());
        assert!(image_at("two\nlines").expect("read").is_none());
        assert!(image_at("").expect("read").is_none());
        // A missing path is not an error; it is just text.
        assert!(
            image_at("C:/definitely/not/here.png")
                .expect("read")
                .is_none()
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_file_url_names_the_same_picture() {
        let path = write_png("url");
        let url = format!("file:///{}", path.display().to_string().replace('\\', "/"));
        let image = image_at(&url).expect("read").expect("image");
        assert_eq!(image.mime, "image/png");
        let _ = std::fs::remove_file(&path);
    }
}
