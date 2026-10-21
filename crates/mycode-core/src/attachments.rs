//! User-message attachments: images the model can see and code/text files
//! carried as fenced text.
//!
//! Limits live here so the composer that reads a dropped file and the core
//! that commits it apply the same caps. The numbers follow what hosted
//! vision APIs accept: 5 MiB of base64 per image (Anthropic, Claude Code,
//! OpenCode's default `max_base64_bytes`), so the raw cap is 3.75 MB and the
//! encoded payload stays under it. Text files are bounded per file and in
//! total because they travel in the prompt on every turn.

use serde::{Deserialize, Serialize};

use crate::message::{BinaryData, ContentBlock, TextBlock, UserMessage};

/// Images one message may carry. Hosted APIs accept more, but a turn that
/// resends four images every step already costs real tokens.
pub const MAX_ATTACHED_IMAGES: usize = 4;
/// Raw image bytes. Base64 inflates the payload (3 bytes become 4), so this
/// stays under 5 MiB encoded.
pub const MAX_IMAGE_BYTES: usize = 3_750_000;
/// Encoded-image ceiling; a caller that already holds base64 checks this.
pub const MAX_IMAGE_BASE64_BYTES: usize = 5 * 1024 * 1024;
/// Encoded ceiling for all pictures of one message. The durable user event
/// that stores them is capped at 8 MiB, and four full-size photos would
/// encode to 20 MiB, so the pictures share this budget.
pub const MAX_MESSAGE_IMAGE_BASE64_BYTES: usize = 6 * 1024 * 1024;
/// Text/code files one message may carry.
pub const MAX_ATTACHED_FILES: usize = 8;
/// One text file: past this the prompt is truncated with a visible marker.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// All text files together.
pub const MAX_FILES_BYTES: usize = 512 * 1024;

/// Image types the composer accepts. SVG is deliberately absent: it is markup,
/// and providers do not decode it as a picture.
pub const IMAGE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Media type from an image's leading bytes, independent of its file name.
/// A renamed picture keeps the type the provider decodes.
#[must_use]
pub fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Why an attachment was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachError {
    /// The message already holds [`MAX_ATTACHED_IMAGES`] images.
    TooManyImages,
    /// The image is larger than [`MAX_IMAGE_BYTES`] raw or
    /// [`MAX_IMAGE_BASE64_BYTES`] encoded.
    ImageTooLarge,
    /// The pictures together exceed [`MAX_MESSAGE_IMAGE_BASE64_BYTES`].
    ImagesTooLarge,
    /// The file's media type is not one of [`IMAGE_MEDIA_TYPES`].
    UnsupportedImage,
    /// The message already holds [`MAX_ATTACHED_FILES`] files.
    TooManyFiles,
    /// The file is binary or not UTF-8 text.
    NotText,
    /// The files together are already at [`MAX_FILES_BYTES`].
    FilesTooLarge,
}

/// One image held as base64, exactly as the provider APIs take it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageAttachment {
    /// Display name the transcript shows and the gateway logs.
    pub name: String,
    /// One of [`IMAGE_MEDIA_TYPES`].
    pub mime_type: String,
    /// Base64-encoded bytes.
    pub data: String,
}

/// One text/code file carried in the prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextAttachment {
    /// Display name; also the path the fenced block names.
    pub name: String,
    /// UTF-8 content, already cut to [`MAX_FILE_BYTES`] when needed.
    pub text: String,
    /// Content was cut at [`MAX_FILE_BYTES`]; the fence says so.
    #[serde(default)]
    pub truncated: bool,
}

/// The attachments of one outgoing message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Attachments {
    /// Pictures the model can see.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
    /// Files carried as text.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<TextAttachment>,
}

impl Attachments {
    /// Whether the message carries nothing extra.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.files.is_empty()
    }

    /// Adds one base64 image, enforcing the image caps.
    ///
    /// # Errors
    ///
    /// Returns [`AttachError::UnsupportedImage`], [`AttachError::ImageTooLarge`],
    /// [`AttachError::ImagesTooLarge`], or [`AttachError::TooManyImages`].
    pub fn push_image(
        &mut self,
        name: impl Into<String>,
        mime_type: &str,
        data: impl Into<String>,
    ) -> Result<(), AttachError> {
        if !IMAGE_MEDIA_TYPES.contains(&mime_type) {
            return Err(AttachError::UnsupportedImage);
        }
        if self.images.len() >= MAX_ATTACHED_IMAGES {
            return Err(AttachError::TooManyImages);
        }
        let data = data.into();
        if data.len() > MAX_IMAGE_BASE64_BYTES {
            return Err(AttachError::ImageTooLarge);
        }
        let encoded: usize = self.images.iter().map(|image| image.data.len()).sum();
        if encoded.saturating_add(data.len()) > MAX_MESSAGE_IMAGE_BASE64_BYTES {
            return Err(AttachError::ImagesTooLarge);
        }
        self.images.push(ImageAttachment {
            name: name.into(),
            mime_type: mime_type.to_owned(),
            data,
        });
        Ok(())
    }

    /// Adds one text file, enforcing the file caps. Content longer than
    /// [`MAX_FILE_BYTES`] is cut at a character boundary and marked truncated.
    ///
    /// # Errors
    ///
    /// Returns [`AttachError::TooManyFiles`, `AttachError::FilesTooLarge`, or
    /// `AttachError::NotText`].
    pub fn push_file(
        &mut self,
        name: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<(), AttachError> {
        if self.files.len() >= MAX_ATTACHED_FILES {
            return Err(AttachError::TooManyFiles);
        }
        let used: usize = self.files.iter().map(|file| file.text.len()).sum();
        let room = MAX_FILES_BYTES.saturating_sub(used);
        if room == 0 {
            return Err(AttachError::FilesTooLarge);
        }
        let text = text.into();
        let limit = MAX_FILE_BYTES.min(room);
        let mut truncated = false;
        let text = if text.len() > limit {
            truncated = true;
            cut_at_boundary(&text, limit)
        } else {
            text
        };
        self.files.push(TextAttachment {
            name: name.into(),
            text,
            truncated,
        });
        Ok(())
    }

    /// One line per attachment, shown below the prompt in the transcript.
    #[must_use]
    pub fn transcript_note(&self) -> String {
        let mut lines = Vec::with_capacity(self.images.len() + self.files.len());
        for image in &self.images {
            lines.push(format!("[image] {}", image.name));
        }
        for file in &self.files {
            lines.push(format!("[file] {}", file.name));
        }
        lines.join("\n")
    }

    /// The provider message: the prompt, then each file as a fenced block,
    /// then the images.
    #[must_use]
    pub fn user_message(&self, prompt: &str) -> UserMessage {
        let mut text = prompt.to_owned();
        for file in &self.files {
            text.push_str("\n\n");
            text.push_str(&fenced_file(file));
        }
        let mut content = vec![ContentBlock::Text(TextBlock::new(text))];
        content.extend(self.images.iter().map(|image| {
            ContentBlock::Image(BinaryData {
                data: image.data.clone(),
                mime_type: image.mime_type.clone(),
            })
        }));
        UserMessage { content }
    }
}

/// Durable form of one user message that carries attachments.
///
/// Plain prompts keep their raw UTF-8 bytes, as every older session does.
/// This envelope is only written when attachments exist, and its
/// `$mycodeUser` key makes it unambiguous: a prompt that is itself JSON is
/// never mistaken for one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredUserMessage {
    /// The text the user typed.
    pub prompt: String,
    /// Pictures and files sent beside it.
    #[serde(default, skip_serializing_if = "Attachments::is_empty")]
    pub attachments: Attachments,
}

/// On-disk wrapper that distinguishes this envelope from any prompt text.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEnvelope {
    #[serde(rename = "$mycodeUser")]
    message: StoredUserMessage,
}

impl StoredUserMessage {
    /// Reads the envelope; `None` means the payload is a plain-text prompt.
    /// The `$mycodeUser` key must be present, so JSON a user typed stays text.
    #[must_use]
    pub fn from_payload(payload: &[u8]) -> Option<Self> {
        serde_json::from_slice::<StoredEnvelope>(payload)
            .ok()
            .map(|envelope| envelope.message)
    }

    /// Encodes the durable payload.
    ///
    /// # Errors
    ///
    /// Returns the serializer error (a stored message is always plain data).
    pub fn to_payload(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&StoredEnvelope {
            message: self.clone(),
        })
    }

    /// The provider message this envelope describes.
    #[must_use]
    pub fn user_message(&self) -> UserMessage {
        self.attachments.user_message(&self.prompt)
    }

    /// The transcript text: the prompt plus the attachment note.
    #[must_use]
    pub fn display_text(&self) -> String {
        let note = self.attachments.transcript_note();
        if note.is_empty() {
            self.prompt.clone()
        } else if self.prompt.is_empty() {
            // An attachments-only prompt: the note is the whole message, so
            // it must not lead with a blank line or repeat itself.
            note
        } else {
            format!("{}\n\n{}", self.prompt, note)
        }
    }
}

/// Cuts `text` to at most `limit` bytes at a character boundary.
fn cut_at_boundary(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// One fenced code block naming its file.
fn fenced_file(file: &TextAttachment) -> String {
    let fence = fence_for(&file.text);
    let language = language_hint(&file.name);
    let mut out = format!("File: {}\n{fence}{language}\n", file.name);
    out.push_str(&file.text);
    if file.truncated {
        out.push_str("\n… (truncated)");
    }
    out.push('\n');
    out.push_str(&fence);
    out
}

/// A backtick fence longer than every run in `text`, so a line of backticks or
/// tildes inside the file cannot close the block early.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for character in text.chars() {
        if character == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// Common fence languages. An unknown extension leaves the info string empty,
/// which every renderer accepts.
fn language_hint(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "sh" | "bash" => "bash",
        "ps1" => "powershell",
        "bat" | "cmd" => "bat",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "scss",
        "json" => "json",
        "jsonc" => "jsonc",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "md" | "mdx" => "markdown",
        "csv" => "csv",
        "lua" => "lua",
        "r" => "r",
        "dart" => "dart",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "hs" => "haskell",
        "ml" => "ocaml",
        "clj" => "clojure",
        "scala" => "scala",
        "vim" => "vim",
        "cmake" => "cmake",
        "svelte" => "svelte",
        "vue" => "vue",
        "proto" => "protobuf",
        "gradle" => "groovy",
        "dockerfile" => "dockerfile",
        "makefile" => "makefile",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttachError, Attachments, MAX_ATTACHED_IMAGES, MAX_FILE_BYTES, MAX_FILES_BYTES,
        MAX_MESSAGE_IMAGE_BASE64_BYTES, StoredUserMessage,
    };
    use crate::message::ContentBlock;

    fn image() -> Attachments {
        let mut attachments = Attachments::default();
        attachments
            .push_image("shot.png", "image/png", "aGVsbG8=")
            .unwrap();
        attachments
    }

    #[test]
    fn images_and_files_become_text_then_image_blocks() {
        let mut attachments = image();
        attachments
            .push_file("src/main.rs", "fn main() {}")
            .unwrap();

        let message = attachments.user_message("what does this do?");
        let [ContentBlock::Text(text), ContentBlock::Image(image)] = message.content.as_slice()
        else {
            panic!("expected text then image: {:?}", message.content);
        };
        assert!(text.text.starts_with("what does this do?"));
        assert!(text.text.contains("File: src/main.rs"));
        assert!(text.text.contains("```rust"));
        assert!(text.text.contains("fn main() {}"));
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.data, "aGVsbG8=");
    }

    #[test]
    fn a_fence_outgrows_every_run_in_the_file() {
        let mut attachments = Attachments::default();
        attachments
            .push_file("notes.md", "before\n```\n~~~\n````\nafter")
            .unwrap();
        let text = attachments.user_message("go");
        let ContentBlock::Text(text) = &text.content[0] else {
            panic!("text block");
        };
        // Longest run is 4, so the block opens and closes with 5.
        assert!(text.text.contains("`````markdown"), "{}", text.text);
        assert!(text.text.ends_with("`````"), "{}", text.text);
        assert!(text.text.contains("~~~"), "{}", text.text);
    }

    #[test]
    fn images_share_a_total_budget() {
        let mut attachments = Attachments::default();
        // Each image is under the per-image cap; together they are not.
        let chunk = "A".repeat(MAX_MESSAGE_IMAGE_BASE64_BYTES / 3);
        for index in 0..3 {
            attachments
                .push_image(format!("shot{index}.png"), "image/png", chunk.clone())
                .unwrap();
        }
        assert_eq!(
            attachments.push_image("shot3.png", "image/png", chunk),
            Err(AttachError::ImagesTooLarge)
        );
    }

    #[test]
    fn limits_refuse_extra_images_and_oversized_files() {
        let mut attachments = Attachments::default();
        for index in 0..MAX_ATTACHED_IMAGES {
            attachments
                .push_image(format!("{index}.png"), "image/png", "aGVsbG8=")
                .unwrap();
        }
        assert_eq!(
            attachments.push_image("extra.png", "image/png", "aGVsbG8="),
            Err(AttachError::TooManyImages)
        );
        assert_eq!(
            Attachments::default().push_image("a.svg", "image/svg+xml", "<svg/>"),
            Err(AttachError::UnsupportedImage)
        );

        let mut files = Attachments::default();
        for index in 0..8 {
            files
                .push_file(format!("{index}.rs"), "fn main() {}")
                .unwrap();
        }
        assert_eq!(
            files.push_file("extra.rs", "fn main() {}"),
            Err(AttachError::TooManyFiles)
        );
        assert_eq!(
            Attachments::default().push_file("huge.txt", "x".repeat(MAX_FILES_BYTES)),
            Ok(())
        );
    }

    #[test]
    fn an_oversized_file_is_cut_at_a_character_boundary() {
        let mut attachments = Attachments::default();
        let text = format!("{}汉", "a".repeat(MAX_FILE_BYTES - 1));
        attachments.push_file("big.txt", text).unwrap();
        let file = &attachments.files[0];
        assert!(file.truncated);
        assert!(file.text.len() <= MAX_FILE_BYTES);
        assert!(file.text.is_char_boundary(file.text.len()));
    }

    #[test]
    fn files_stop_at_the_shared_total() {
        let mut attachments = Attachments::default();
        attachments
            .push_file("a.txt", "x".repeat(MAX_FILE_BYTES))
            .unwrap();
        attachments
            .push_file("b.txt", "x".repeat(MAX_FILE_BYTES))
            .unwrap();
        let total: usize = attachments.files.iter().map(|file| file.text.len()).sum();
        assert_eq!(total, MAX_FILES_BYTES);
        assert_eq!(
            attachments.push_file("c.txt", "y"),
            Err(AttachError::FilesTooLarge)
        );
    }

    #[test]
    fn an_attachments_only_message_shows_the_note_once() {
        let stored = StoredUserMessage {
            prompt: String::new(),
            attachments: image(),
        };
        assert_eq!(stored.display_text(), "[image] shot.png");
        let payload = stored.to_payload().expect("payload");
        let decoded = StoredUserMessage::from_payload(&payload).expect("envelope");
        assert_eq!(decoded.display_text(), "[image] shot.png");
    }

    #[test]
    fn the_durable_envelope_round_trips_and_keeps_names() {
        let mut attachments = image();
        attachments.push_file("main.rs", "fn main() {}").unwrap();
        let stored = StoredUserMessage {
            prompt: "look".to_owned(),
            attachments,
        };
        let bytes = stored.to_payload().unwrap();
        let decoded = StoredUserMessage::from_payload(&bytes).expect("envelope");
        assert_eq!(decoded, stored);
        assert!(decoded.display_text().contains("[image] shot.png"));
        assert!(decoded.display_text().contains("[file] main.rs"));
        assert_eq!(decoded.user_message().content.len(), 2);
        assert!(StoredUserMessage::from_payload(b"plain prompt").is_none());
        // A prompt that is itself JSON stays text: only the tagged envelope
        // is read back as a message.
        assert!(StoredUserMessage::from_payload(br#"{"text":"just json"}"#).is_none());
        assert!(StoredUserMessage::from_payload(br#"{"prompt":"hi"}"#).is_none());
        assert!(StoredUserMessage::from_payload(br#"{"prompt":"hi","attachments":{}}"#).is_none());
        assert!(!String::from_utf8(bytes).unwrap().is_empty());
    }
}
