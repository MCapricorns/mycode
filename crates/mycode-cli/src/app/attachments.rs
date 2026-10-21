//! Clipboard pictures in the terminal composer.
//!
//! `Ctrl+V` attaches the picture the clipboard holds, and a paste whose whole
//! text is one image path attaches that file instead of typing the path. The
//! limits and the provider message come from [`mycode_core::attachments`], so
//! the terminal and the desktop send the same thing. `Ctrl+X` clears what is
//! waiting.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use mycode_core::{AttachError, MAX_IMAGE_BYTES};
use mycode_frontend::driver::ToastKind;

use super::Tui;
use super::clipboard;

impl Tui {
    /// `Ctrl+V`: attaches the clipboard picture, or says there is none.
    pub(super) fn attach_clipboard_image(&mut self) {
        match clipboard::read() {
            Ok(Some(image)) => {
                let name = self.next_image_name();
                self.attach_image(name, image.bytes, image.mime);
            }
            Ok(None) => {
                self.push_notice(ToastKind::Info, "No picture in the clipboard.".to_owned())
            }
            Err(error) => self.push_notice(ToastKind::Warning, error),
        }
        self.dirty = true;
    }

    /// A paste that names one image file attaches it. Returns whether the
    /// paste was consumed, so the text is not typed as well.
    pub(super) fn attach_pasted_image(&mut self, text: &str) -> bool {
        let image = match clipboard::image_at(text) {
            Ok(Some(image)) => image,
            Ok(None) => return false,
            Err(error) => {
                self.push_notice(ToastKind::Warning, error);
                self.dirty = true;
                return true;
            }
        };
        let name = self.image_name_for(&clipboard::local_path(text));
        self.attach_image(name, image.bytes, image.mime);
        self.dirty = true;
        true
    }

    /// `Ctrl+X`: drops every waiting attachment.
    pub(super) fn clear_attachments(&mut self) {
        if self.attachments.is_empty() {
            return;
        }
        self.attachments = mycode_core::Attachments::default();
        self.push_notice(ToastKind::Info, "Attachments cleared.".to_owned());
        self.dirty = true;
    }

    /// Puts attachments a failed send handed back into the composer.
    pub(super) fn restore_attachments(&mut self, attachments: mycode_core::Attachments) {
        for image in attachments.images {
            let name = image.name.clone();
            if let Err(error) =
                self.attachments
                    .push_image(image.name, &image.mime_type, image.data)
            {
                self.push_notice(
                    ToastKind::Warning,
                    format!("{name}: {}", attachment_reason(error)),
                );
            }
        }
        for file in attachments.files {
            let name = file.name.clone();
            match self.attachments.push_file(file.name, file.text) {
                Ok(()) => {
                    if file.truncated
                        && let Some(last) = self.attachments.files.last_mut()
                    {
                        last.truncated = true;
                    }
                }
                Err(error) => self.push_notice(
                    ToastKind::Warning,
                    format!("{name}: {}", attachment_reason(error)),
                ),
            }
        }
        self.dirty = true;
    }

    fn attach_image(&mut self, name: String, bytes: Vec<u8>, mime: &'static str) {
        if !self.takes_images() {
            self.push_notice(
                ToastKind::Warning,
                format!("{name}: the selected model cannot see images"),
            );
            return;
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            self.push_notice(
                ToastKind::Warning,
                format!("{name}: image is larger than 3.75 MB"),
            );
            return;
        }
        let data = BASE64_STANDARD.encode(bytes);
        match self.attachments.push_image(name.clone(), mime, data) {
            Ok(()) => {
                let count = self.attachments.images.len();
                let noun = if count == 1 { "image" } else { "images" };
                self.push_notice(
                    ToastKind::Info,
                    format!("Attached {name} ({count} {noun} waiting)."),
                );
            }
            Err(error) => self.push_notice(
                ToastKind::Warning,
                format!("{name}: {}", attachment_reason(error)),
            ),
        }
    }

    /// Whether the selected catalog model advertises image input. A catalog
    /// entry that has not loaded yet is unknown, so the provider decides.
    pub(super) fn takes_images(&self) -> bool {
        let vm = self.driver.vm();
        let Some(provider) = vm.selected_provider.as_deref() else {
            return true;
        };
        let Some(model) = vm.selected_model.as_deref() else {
            return true;
        };
        vm.catalog
            .as_ref()
            .and_then(|catalog| catalog.model(provider, model))
            .is_none_or(|model| model.attachment)
    }

    fn next_image_name(&self) -> String {
        let count = self.attachments.images.len() + 1;
        if count == 1 {
            "clipboard.png".to_owned()
        } else {
            format!("clipboard-{count}.png")
        }
    }

    fn image_name_for(&self, path: &std::path::Path) -> String {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "clipboard.png".to_owned())
    }
}

/// Why an attachment was refused, in the terminal's one-line style.
fn attachment_reason(error: AttachError) -> &'static str {
    match error {
        AttachError::TooManyImages => "too many images: at most 4 per message",
        AttachError::ImageTooLarge => "image is larger than 3.75 MB",
        AttachError::ImagesTooLarge => "the pictures together exceed 6 MB",
        AttachError::UnsupportedImage => "unsupported image type",
        AttachError::TooManyFiles => "too many files: at most 8 per message",
        AttachError::NotText => "only text or code files",
        AttachError::FilesTooLarge => "attached files together exceed 512 KB",
    }
}
