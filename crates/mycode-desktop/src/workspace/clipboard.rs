//! Clipboard pictures for the composer.
//!
//! A paste whose clipboard holds a picture attaches it to the next prompt
//! instead of typing anything; a paste whose text is one image path attaches
//! that file. gpui reads the platform clipboard, so PNG, JPEG, WebP, and GIF
//! bytes are used as they are, and a bitmap (a Windows screenshot) or a TIFF
//! (a macOS one) is re-encoded as PNG because providers decode neither. A
//! picture that cannot be used reports why; a clipboard that holds only text
//! stays text.

use std::path::{Path, PathBuf};

use gpui_kit::{ClipboardEntry, ClipboardItem, ImageFormat};
use mycode_core::MAX_IMAGE_BYTES;

use crate::i18n::t;

/// One picture read out of the clipboard or a pasted path.
pub(super) struct ClipboardPicture {
    /// Encoded bytes in the format `mime` names.
    pub(super) bytes: Vec<u8>,
    /// One of [`mycode_core::IMAGE_MEDIA_TYPES`].
    pub(super) mime: &'static str,
}

impl ClipboardPicture {
    /// Display name for the attachment chip.
    pub(super) fn file_name(&self) -> String {
        let extension = match self.mime {
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        };
        format!("image.{extension}")
    }

    /// The picture as the composer stores it: base64 in `mime`'s format.
    pub(super) fn base64(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&self.bytes)
    }
}

/// Reads the picture a paste carries: the clipboard's own picture, or the
/// image file its text names.
///
/// # Errors
///
/// Returns a message when a picture is present but cannot be used.
pub(super) fn pasted(item: &ClipboardItem) -> Result<Option<ClipboardPicture>, String> {
    if let Some(picture) = clipboard_picture(item)? {
        return Ok(Some(picture));
    }
    match item.text() {
        Some(text) => picture_at(&text),
        None => Ok(None),
    }
}

fn clipboard_picture(item: &ClipboardItem) -> Result<Option<ClipboardPicture>, String> {
    let mut unusable = false;
    for entry in item.entries() {
        let ClipboardEntry::Image(image) = entry else {
            continue;
        };
        if image.bytes.len() > MAX_IMAGE_BYTES {
            return Err(t("image is larger than 3.75 MB", "图片超过 3.75 MB").to_owned());
        }
        if let Some(mime) = mycode_core::image_media_type(&image.bytes) {
            return Ok(Some(ClipboardPicture {
                bytes: image.bytes.clone(),
                mime,
            }));
        }
        if matches!(image.format, ImageFormat::Bmp | ImageFormat::Tiff)
            && let Some(bytes) = reencoded_as_png(&image.bytes)
        {
            return Ok(Some(ClipboardPicture {
                bytes,
                mime: "image/png",
            }));
        }
        unusable = true;
    }
    if unusable {
        return Err(t("unsupported image type", "不支持这种图片格式").to_owned());
    }
    Ok(None)
}

/// Reads one image file referenced by pasted text, if `text` names one.
///
/// Quoted paths (`"C:\Shots\a b.png"`) and `file://` URLs are accepted; any
/// other text belongs to the composer. Oversized files are refused so a
/// paste cannot block the UI on a huge read.
fn picture_at(text: &str) -> Result<Option<ClipboardPicture>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) {
        return Ok(None);
    }
    let path = local_path(trimmed);
    if !path.is_file() {
        return Ok(None);
    }
    read_file(&path).map(Some)
}

fn read_file(path: &Path) -> Result<ClipboardPicture, String> {
    let length = std::fs::metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    if length > MAX_IMAGE_BYTES as u64 {
        return Err(format!(
            "{}: {}",
            path.display(),
            t("image is larger than 3.75 MB", "图片超过 3.75 MB")
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mime = mycode_core::image_media_type(&bytes).ok_or_else(|| {
        format!(
            "{}: {}",
            path.display(),
            t("unsupported image type", "不支持这种图片格式")
        )
    })?;
    Ok(ClipboardPicture { bytes, mime })
}

/// Re-encodes a bitmap or a TIFF as PNG. `None` when the bytes are not a
/// picture the decoder can read.
fn reencoded_as_png(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Cursor;

    let picture = image::load_from_memory(bytes).ok()?;
    let mut png = Vec::new();
    picture
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// The local path a pasted path or `file://` URL names.
fn local_path(text: &str) -> PathBuf {
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
/// of the file name.
fn file_url_path(rest: &str) -> String {
    let bytes = rest.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        return rest[1..].to_owned();
    }
    rest.to_owned()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{local_path, picture_at, reencoded_as_png};

    /// The leading bytes are all the path reader checks; the picture itself
    /// is never decoded.
    fn write_png(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mycode-paste-{label}-{}-{}.png",
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
        let picture = picture_at(&text).expect("read").expect("picture");
        assert_eq!(picture.mime, "image/png");
        assert_eq!(picture.file_name(), "image.png");
        // A path with spaces is often pasted quoted; the quotes are not the
        // name.
        let quoted = format!("\"{text}\"");
        assert!(picture_at(&quoted).expect("read").is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ordinary_text_stays_text() {
        assert!(picture_at("hello world").expect("read").is_none());
        assert!(picture_at("two\nlines").expect("read").is_none());
        assert!(picture_at("").expect("read").is_none());
        assert!(
            picture_at("C:/definitely/not/here.png")
                .expect("read")
                .is_none()
        );
    }

    #[test]
    fn a_file_url_names_the_same_picture() {
        let path = write_png("url");
        let url = format!("file:///{}", path.display().to_string().replace('\\', "/"));
        assert!(picture_at(&url).expect("read").is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_windows_drive_url_keeps_its_letter() {
        assert_eq!(
            local_path("file:///C:/x/a.png"),
            PathBuf::from("C:/x/a.png")
        );
        assert_eq!(
            local_path("file://localhost/C:/x/a.png"),
            PathBuf::from("C:/x/a.png")
        );
    }

    /// A 2x2 32-bit BI_RGB bitmap, the shape a screenshot tool publishes.
    fn tiny_bmp() -> Vec<u8> {
        let header_size = 40u32;
        let mut dib = Vec::new();
        dib.extend_from_slice(&header_size.to_le_bytes());
        dib.extend_from_slice(&2i32.to_le_bytes());
        dib.extend_from_slice(&2i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.extend_from_slice(&16u32.to_le_bytes());
        dib.extend_from_slice(&[0u8; 16]);
        for color in [
            [0, 0, 255, 0],
            [0, 255, 0, 0],
            [255, 0, 0, 0],
            [255, 255, 255, 0],
        ] {
            dib.extend_from_slice(&color);
        }
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(14 + dib.len() as u32).to_le_bytes());
        bmp.extend_from_slice(&[0u8; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&dib);
        bmp
    }

    #[test]
    fn a_bitmap_is_reencoded_as_png() {
        let png = reencoded_as_png(&tiny_bmp()).expect("png");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert_eq!(mycode_core::image_media_type(&png), Some("image/png"));
    }
}
