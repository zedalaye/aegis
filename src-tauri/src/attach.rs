//! Images the operator attaches to a message (PLAN 7.20).
//!
//! * [`import`] copies picked or dropped files into the app's own attachment
//!   directory — never the workspace — after checking their bytes are an image
//!   a model takes, under [`MAX_BYTES`].
//! * [`resolve`] turns the ids the window sends back with `session_send` into
//!   [`Attachment`] records. The window never sends a path or a byte.
//!
//! The directory is on the `asset:` scope from startup (`lib.rs`), which is how
//! the bubble draws an attachment without its bytes crossing `invoke`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::store::Attachment;

/// Where attachments live, beside the stores.
pub const DIR: &str = "attachments";

/// Largest file taken, the explorer's image cap.
pub const MAX_BYTES: u64 = crate::explorer::IMAGE_MAX_BYTES;

/// How many images one message may carry.
pub const MAX_PER_MESSAGE: usize = 8;

/// The types a model takes, and the extension each copy is stored under.
const TYPES: [(&str, &str); 4] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
];

/// One file copied in, as the composer shows it before the send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Attached {
    /// What `session_send` names it by: the copy's file name.
    pub id: String,
    /// The original file's name, for the chip.
    pub name: String,
    /// Size of the copy, in bytes.
    #[ts(type = "number")]
    pub bytes: u64,
    /// What will be stored on the message.
    pub attachment: Attachment,
}

/// A file that was not attached, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct NotAttached {
    /// The original file's name.
    pub name: String,
    /// Why, in words somebody can act on.
    pub reason: String,
}

/// What one pick or drop did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct AttachReport {
    /// Copied in, in the order given.
    pub attached: Vec<Attached>,
    /// Refused, in the order given.
    pub refused: Vec<NotAttached>,
}

/// Copies each source into `dir`. Per-file refusals are in the report.
pub fn import(dir: &Path, sources: &[PathBuf]) -> AttachReport {
    let mut report = AttachReport::default();
    for source in sources {
        let name = source.file_name().map_or_else(
            || source.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        if report.attached.len() >= MAX_PER_MESSAGE {
            report.refused.push(NotAttached {
                name,
                reason: format!("a message takes at most {MAX_PER_MESSAGE} images"),
            });
            continue;
        }
        match import_one(dir, source) {
            Ok(attached) => report.attached.push(Attached { name, ..attached }),
            Err(reason) => report.refused.push(NotAttached { name, reason }),
        }
    }
    report
}

fn import_one(dir: &Path, source: &Path) -> Result<Attached, String> {
    let meta = fs::metadata(source).map_err(|_| "it could not be read".to_owned())?;
    if meta.is_dir() {
        return Err("it is a folder".to_owned());
    }
    if meta.len() > MAX_BYTES {
        return Err(format!(
            "it is larger than {} MB",
            MAX_BYTES / (1024 * 1024)
        ));
    }

    let mut bytes = Vec::new();
    fs::File::open(source)
        .and_then(|file| file.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| "it could not be read".to_owned())?;

    let (mime, ext) =
        kind(&bytes).ok_or_else(|| "it is not a PNG, JPEG, GIF or WebP image".to_owned())?;
    let (width, height) = dimensions(&bytes).unzip();

    fs::create_dir_all(dir).map_err(|err| {
        tracing::error!(%err, dir = %dir.display(), "attachment directory unavailable");
        "the attachment folder could not be created".to_owned()
    })?;
    let id = format!("{}.{ext}", uuid::Uuid::new_v4());
    let path = dir.join(&id);
    fs::write(&path, &bytes).map_err(|err| {
        tracing::error!(%err, "attachment not written");
        "the copy could not be written".to_owned()
    })?;

    Ok(Attached {
        id,
        name: String::new(),
        bytes: bytes.len() as u64,
        attachment: Attachment {
            path: path.display().to_string(),
            mime: mime.to_owned(),
            width,
            height,
        },
    })
}

/// The records for the ids the window sent. Refuses an id that is not a copy
/// in `dir` — the whole send, not one image.
pub fn resolve(dir: &Path, ids: &[String]) -> AppResult<Vec<Attachment>> {
    if ids.len() > MAX_PER_MESSAGE {
        return Err(AppError::Attach {
            reason: format!("a message takes at most {MAX_PER_MESSAGE} images"),
        });
    }
    ids.iter().map(|id| resolve_one(dir, id)).collect()
}

fn resolve_one(dir: &Path, id: &str) -> AppResult<Attachment> {
    let refuse = |reason: &str| AppError::Attach {
        reason: reason.to_owned(),
    };
    // Exactly `<uuid>.<ext>`: no separator, no `..`, nothing but a copy made
    // by `import`.
    let (stem, ext) = id
        .split_once('.')
        .ok_or_else(|| refuse("the runtime did not make that copy"))?;
    if uuid::Uuid::parse_str(stem).is_err() || !TYPES.iter().any(|(_, known)| *known == ext) {
        return Err(refuse("the runtime did not make that copy"));
    }

    let path = dir.join(id);
    let bytes = read_head(&path).map_err(|_| refuse("the copy is no longer there"))?;
    let (mime, _) = kind(&bytes).ok_or_else(|| refuse("the copy is not an image"))?;
    let (width, height) = dimensions_of(&path).unzip();

    Ok(Attachment {
        path: path.display().to_string(),
        mime: mime.to_owned(),
        width,
        height,
    })
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(32);
    fs::File::open(path)?.take(32).read_to_end(&mut head)?;
    Ok(head)
}

/// The type and extension the bytes declare, among the ones a model takes.
fn kind(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    let sniffed = crate::explorer::sniff(bytes)?;
    TYPES.iter().copied().find(|(mime, _)| *mime == sniffed)
}

fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn dimensions_of(path: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::new(width, height)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode");
        out
    }

    #[test]
    fn an_image_is_copied_in_and_resolves_by_id() {
        let src = TempDir::new().expect("src");
        let dir = TempDir::new().expect("dir");
        let file = src.path().join("shot.png");
        fs::write(&file, png(3, 2)).expect("write");

        let report = import(dir.path(), &[file]);
        assert!(report.refused.is_empty(), "{report:?}");
        let attached = &report.attached[0];
        assert_eq!(attached.name, "shot.png");
        assert!(attached.id.ends_with(".png"));
        assert_eq!(attached.attachment.mime, "image/png");
        assert_eq!(attached.attachment.width, Some(3));
        assert_eq!(attached.attachment.height, Some(2));

        let resolved = resolve(dir.path(), std::slice::from_ref(&attached.id)).expect("resolve");
        assert_eq!(resolved, vec![attached.attachment.clone()]);
    }

    #[test]
    fn a_name_is_not_evidence() {
        let src = TempDir::new().expect("src");
        let dir = TempDir::new().expect("dir");
        let fake = src.path().join("fake.png");
        fs::write(&fake, "just words").expect("write");

        let report = import(dir.path(), &[fake, src.path().to_path_buf()]);
        assert!(report.attached.is_empty());
        assert_eq!(report.refused.len(), 2);
        assert_eq!(
            report.refused[0].reason,
            "it is not a PNG, JPEG, GIF or WebP image"
        );
        assert_eq!(report.refused[1].reason, "it is a folder");
    }

    #[test]
    fn only_a_copy_made_here_resolves() {
        let dir = TempDir::new().expect("dir");
        for id in [
            "../secrets.png",
            "shot.png",
            "00000000-0000-0000-0000-000000000000.exe",
            "00000000-0000-0000-0000-000000000000.png/../x",
            "00000000-0000-0000-0000-000000000000.png",
        ] {
            assert!(
                matches!(
                    resolve(dir.path(), &[id.to_owned()]),
                    Err(AppError::Attach { .. })
                ),
                "{id}"
            );
        }
    }

    #[test]
    fn a_message_holds_a_bounded_number_of_images() {
        let ids = vec![String::new(); MAX_PER_MESSAGE + 1];
        assert!(matches!(
            resolve(Path::new("."), &ids),
            Err(AppError::Attach { .. })
        ));
    }
}
