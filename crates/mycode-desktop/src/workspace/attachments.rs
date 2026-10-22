//! Pictures and code files waiting for the next prompt: dropping them,
//! pasting them, and the limits both paths share.
//!
//! A file dropped on the composer attaches to the next prompt instead of
//! switching the project, and a pasted picture attaches the same way. Limits
//! mirror what hosted vision APIs accept; see [`mycode_frontend::attachments`]. A
//! folder dropped on the composer binds as the project.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use gpui_kit::Context;

use mycode_frontend::{AttachError, MAX_FILE_BYTES, MAX_IMAGE_BYTES};

use super::Workspace;
use super::clipboard;
use super::toasts::ToastKind;
use crate::i18n::t;

/// Image extensions the composer reads. SVG stays a document: providers do
/// not decode it as a picture.
const IMAGE_EXTENSIONS: [(&str, &str); 5] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
];

impl Workspace {
    /// Reads dropped paths into the next prompt, or binds a dropped folder.
    pub(crate) fn on_drop_attachments(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let mut folders = Vec::new();
        for path in paths {
            if path.is_dir() {
                folders.push(path.clone());
                continue;
            }
            self.attach_one(path, cx);
        }
        if !folders.is_empty() {
            self.on_drop_project(&folders, cx);
        }
        cx.notify();
    }

    /// Detaches one waiting attachment, by image or file index.
    pub(crate) fn on_remove_attachment(
        &mut self,
        image_index: Option<usize>,
        file_index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = image_index
            && index < self.attachments.images.len()
        {
            self.attachments.images.remove(index);
        }
        if let Some(index) = file_index
            && index < self.attachments.files.len()
        {
            self.attachments.files.remove(index);
        }
        cx.notify();
    }

    fn attach_one(&mut self, path: &Path, cx: &mut Context<Self>) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if IMAGE_EXTENSIONS
            .iter()
            .any(|(known, _)| known == &extension.as_str())
        {
            if !self.selected_model_takes_images() {
                self.push_toast(
                    format!(
                        "{name}: {}",
                        t(
                            "the selected model cannot see images; pick a vision model first",
                            "当前模型看不了图片，请先选择支持图片的模型"
                        )
                    ),
                    ToastKind::Warning,
                    cx,
                );
                return;
            }
            match read_image(path) {
                Ok((mime, data)) => self.push_picture(name, mime, data, cx),
                Err(error) => self.push_toast(error, ToastKind::Warning, cx),
            }
            return;
        }
        match read_text_file(path) {
            Ok((text, truncated)) => match self.attachments.push_file(name.clone(), text) {
                Ok(()) => {
                    // The reader stops at the cap before `push_file` sees the
                    // excess, so mark the attachment here too.
                    if truncated && let Some(file) = self.attachments.files.last_mut() {
                        file.truncated = true;
                    }
                }
                Err(error) => self.toast_attachment_error(&name, error, cx),
            },
            Err(error) => self.push_toast(error, ToastKind::Warning, cx),
        }
    }

    /// Attaches the picture a paste carries. `true` means the paste was
    /// consumed, so the composer inserts no text for it.
    pub(crate) fn on_composer_paste(
        &mut self,
        item: &gpui_kit::ClipboardItem,
        cx: &mut Context<Self>,
    ) -> bool {
        match clipboard::pasted(item) {
            Ok(Some(picture)) => {
                self.push_picture(picture.file_name(), picture.mime, picture.base64(), cx);
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.push_toast(error, ToastKind::Warning, cx);
                true
            }
        }
    }

    /// Adds one encoded picture, refusing it when the selected model cannot
    /// see pictures at all.
    fn push_picture(
        &mut self,
        name: String,
        mime: &'static str,
        data: String,
        cx: &mut Context<Self>,
    ) {
        if !self.selected_model_takes_images() {
            let reason = t(
                "the selected model cannot see images; pick a vision model first",
                "当前模型看不了图片，请先选择支持图片的模型",
            );
            self.push_toast(format!("{name}: {reason}"), ToastKind::Warning, cx);
            return;
        }
        if let Err(error) = self.attachments.push_image(name.clone(), mime, data) {
            self.toast_attachment_error(&name, error, cx);
        }
        cx.notify();
    }

    /// Whether the selected catalog model advertises image input. A catalog
    /// entry that has not loaded yet is unknown, so the provider decides.
    pub(super) fn selected_model_takes_images(&self) -> bool {
        let Some(provider) = self.vm().selected_provider.as_deref() else {
            return true;
        };
        let Some(model) = self.vm().selected_model.as_deref() else {
            return true;
        };
        self.vm()
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.model(provider, model))
            .is_none_or(|model| model.attachment)
    }

    /// Puts attachments a failed send handed back into the composer.
    pub(crate) fn on_attachments_restored(
        &mut self,
        attachments: mycode_frontend::Attachments,
        cx: &mut Context<Self>,
    ) {
        for image in attachments.images {
            let name = image.name.clone();
            if let Err(error) =
                self.attachments
                    .push_image(image.name, &image.mime_type, image.data)
            {
                self.toast_attachment_error(&name, error, cx);
            }
        }
        for file in attachments.files {
            let name = file.name.clone();
            match self.attachments.push_file(file.name, file.text) {
                Ok(()) => {
                    // `push_file` sees the text alone; the reader marked the
                    // cut before the send, so carry the flag back.
                    if file.truncated
                        && let Some(last) = self.attachments.files.last_mut()
                    {
                        last.truncated = true;
                    }
                }
                Err(error) => self.toast_attachment_error(&name, error, cx),
            }
        }
        cx.notify();
    }

    fn toast_attachment_error(&mut self, name: &str, error: AttachError, cx: &mut Context<Self>) {
        let reason = match error {
            AttachError::TooManyImages => t(
                "too many images: at most 4 per message",
                "图片太多了：每条消息最多 4 张",
            ),
            AttachError::ImageTooLarge => t("image is larger than 3.75 MB", "图片超过 3.75 MB"),
            AttachError::ImagesTooLarge => {
                t("the pictures together exceed 6 MB", "图片总计超过 6 MB")
            }
            AttachError::UnsupportedImage => t("unsupported image type", "不支持这种图片格式"),
            AttachError::TooManyFiles => t(
                "too many files: at most 8 per message",
                "文件太多了：每条消息最多 8 个",
            ),
            AttachError::NotText => t("only text or code files", "只能附加文本或代码文件"),
            AttachError::FilesTooLarge => t(
                "attached files together exceed 512 KB",
                "附件总计超过 512 KB",
            ),
        };
        self.push_toast(format!("{name}: {reason}"), ToastKind::Warning, cx);
    }
}

/// Reads one image as base64, refusing anything over the raw cap. The media
/// type comes from the bytes, not the extension: a renamed JPEG is still a
/// JPEG to the provider.
fn read_image(path: &Path) -> Result<(&'static str, String), String> {
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
    let mime = mycode_frontend::image_media_type(&bytes).ok_or_else(|| {
        format!(
            "{}: {}",
            path.display(),
            t("unsupported image type", "不支持这种图片格式")
        )
    })?;
    Ok((mime, BASE64_STANDARD.encode(bytes)))
}

/// Reads one text file, stopping at the per-file cap and reporting whether
/// there was more. A cut that lands inside a multi-byte character drops only
/// the partial character; a file that is genuinely not UTF-8 is refused.
fn read_text_file(path: &Path) -> Result<(String, bool), String> {
    use std::io::Read as _;

    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::with_capacity(MAX_FILE_BYTES + 4);
    file.take((MAX_FILE_BYTES + 4) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err(format!(
            "{}: {}",
            path.display(),
            t(
                "binary file; only text or code",
                "二进制文件；只支持文本或代码"
            )
        ));
    }
    let truncated = bytes.len() > MAX_FILE_BYTES;
    let cut = MAX_FILE_BYTES.min(bytes.len());
    match std::str::from_utf8(&bytes[..cut]) {
        Ok(text) => Ok((text.to_owned(), truncated)),
        // Only the character straddling the cap may be dropped. Any other
        // invalid sequence means the file is not UTF-8 text.
        Err(error) if truncated && error.valid_up_to() + 4 > cut => {
            let valid = error.valid_up_to();
            let text = std::str::from_utf8(&bytes[..valid])
                .expect("the prefix before an invalid sequence is valid");
            Ok((text.to_owned(), true))
        }
        Err(_) => Err(format!(
            "{}: {}",
            path.display(),
            t("not UTF-8 text", "不是 UTF-8 文本")
        )),
    }
}
