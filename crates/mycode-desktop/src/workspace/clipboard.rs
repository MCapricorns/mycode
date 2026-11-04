//! Clipboard pictures for the composer.
//!
//! A paste whose clipboard holds a picture attaches it to the next prompt
//! instead of typing anything; a paste whose text is one image path attaches
//! that file. gpui reads the platform clipboard, so PNG, JPEG, WebP, and GIF
//! bytes are used as they are, and a bitmap (a Windows screenshot) or a TIFF
//! (a macOS one) is re-encoded as PNG because providers decode neither. A
//! picture that cannot be used reports why; a clipboard that holds only text
//! stays text.

use std::path::PathBuf;

use gpui_kit::{ClipboardEntry, ClipboardItem, ImageFormat};
use mycode::core::MAX_IMAGE_BYTES;

use crate::i18n::t;

// Native clipboard screenshots are uncompressed (a 4K BMP is about 32 MiB).
// Bound their decoding separately from the encoded attachment sent to a model.
const MAX_CLIPBOARD_BITMAP_BYTES: usize = 64 * 1024 * 1024;

/// One picture read out of the clipboard or a pasted path.
pub(super) struct ClipboardPicture {
    /// Encoded bytes in the format `mime` names.
    pub(super) bytes: Vec<u8>,
    /// One of [`mycode::core::IMAGE_MEDIA_TYPES`].
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
        if let Some(picture) = picture_bytes(&image.bytes, &image.format)? {
            return Ok(Some(picture));
        }
        unusable = true;
    }
    if unusable {
        return Err(t("unsupported image type", "不支持这种图片格式").to_owned());
    }
    Ok(None)
}

fn picture_bytes(bytes: &[u8], format: &ImageFormat) -> Result<Option<ClipboardPicture>, String> {
    if let Some(mime) = mycode::core::image_media_type(bytes) {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(t("image is larger than 3.75 MB", "图片超过 3.75 MB").to_owned());
        }
        return Ok(Some(ClipboardPicture {
            bytes: bytes.to_vec(),
            mime,
        }));
    }
    if matches!(format, ImageFormat::Bmp | ImageFormat::Tiff) {
        let bytes = reencoded_as_png(bytes)?;
        return Ok(Some(ClipboardPicture {
            bytes,
            mime: "image/png",
        }));
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
    let Some(path) = local_path(trimmed) else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }
    // Existing source/document paths are ordinary composer text. Probe only
    // the signature before the bounded image read, so even a large text file
    // is not consumed as an oversized or unsupported picture.
    use std::io::Read as _;
    let mut signature = Vec::with_capacity(12);
    std::fs::File::open(&path)
        .and_then(|file| file.take(12).read_to_end(&mut signature))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if mycode::core::image_media_type(&signature).is_none() {
        return Ok(None);
    }
    let (mime, bytes) = super::attachments::read_image_file(&path)?;
    Ok(Some(ClipboardPicture { bytes, mime }))
}

/// Re-encodes a bitmap or TIFF within a bounded decode budget, then applies
/// the attachment limit to the PNG that will actually be sent.
fn reencoded_as_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Cursor;

    let decode_error = || {
        t(
            "clipboard picture is invalid or too large to decode",
            "剪贴板图片无效或过大，无法解码",
        )
        .to_owned()
    };
    if bytes.len() > MAX_CLIPBOARD_BITMAP_BYTES {
        return Err(decode_error());
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| decode_error())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(MAX_CLIPBOARD_BITMAP_BYTES as u64);
    reader.limits(limits);
    let picture = reader.decode().map_err(|_| decode_error())?;
    let mut png = Vec::new();
    picture
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|_| decode_error())?;
    if png.len() > MAX_IMAGE_BYTES {
        return Err(t("image is larger than 3.75 MB", "图片超过 3.75 MB").to_owned());
    }
    Ok(png)
}

/// The local path a pasted path or `file://` URL names.
fn local_path(text: &str) -> Option<PathBuf> {
    let trimmed = text.trim().trim_matches(['"', '\'']);
    let Some(rest) = trimmed.strip_prefix("file://") else {
        return Some(PathBuf::from(trimmed));
    };
    let (host, path) = rest.split_once('/')?;
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    // URL paths encode spaces, Unicode and literal percent signs. Decode
    // bytes before UTF-8 validation so a multibyte filename stays intact.
    let mut decoded = Vec::with_capacity(path.len() + 1);
    decoded.push(b'/');
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        };
        if byte == 0 {
            return None;
        }
        decoded.push(byte);
    }
    Some(PathBuf::from(file_url_path(
        &String::from_utf8(decoded).ok()?,
    )))
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

    use super::{local_path, picture_at, picture_bytes, reencoded_as_png};

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
            Some(PathBuf::from("C:/x/a.png"))
        );
        assert_eq!(
            local_path("file://localhost/C:/x/a.png"),
            Some(PathBuf::from("C:/x/a.png"))
        );
    }

    #[test]
    fn pasted_source_paths_stay_text_even_when_the_file_is_large() {
        let path = write_png("source");
        std::fs::write(&path, vec![b'x'; mycode::core::MAX_IMAGE_BYTES + 1]).expect("source file");
        assert!(
            picture_at(&path.display().to_string())
                .expect("read")
                .is_none()
        );
        std::fs::remove_file(path).expect("cleanup");
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
        assert_eq!(mycode::core::image_media_type(&png), Some("image/png"));
    }

    #[test]
    fn a_native_screenshot_is_limited_after_png_conversion() {
        let screenshot = image::RgbImage::from_pixel(1200, 1200, image::Rgb([30, 40, 50]));
        let mut bmp = Vec::new();
        screenshot
            .write_to(&mut std::io::Cursor::new(&mut bmp), image::ImageFormat::Bmp)
            .expect("encode screenshot");
        assert!(bmp.len() > mycode::core::MAX_IMAGE_BYTES);
        let picture = picture_bytes(&bmp, &gpui_kit::ImageFormat::Bmp)
            .expect("native screenshot")
            .expect("picture");
        assert_eq!(picture.mime, "image/png");
        assert!(picture.bytes.len() < mycode::core::MAX_IMAGE_BYTES);
    }
}
