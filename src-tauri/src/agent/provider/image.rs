//! Reading the images a request names (PLAN 7.20).
//!
//! The transcript names files; [`load`] reads them just before a provider
//! writes its body, off the async threads. A file too large for providers is
//! downscaled, then re-encoded as JPEG if that is still not enough. A file that
//! is gone, or will not fit, is sent as a note instead — the turn goes on.
//!
//! Re-reading every round is cheap for a small file; fitting a large capture
//! is not, so fitted results are kept for the life of the process, keyed on the
//! file's path, size and modification time.

use std::collections::VecDeque;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use ::image::{DynamicImage, ImageFormat, ImageReader};
use base64::Engine as _;

use crate::agent::wire::{ModelRequest, WireImage, WireMessage};

/// Longest side sent. Larger images are resized by the providers anyway, at
/// the cost of the upload.
pub const MAX_SIDE: u32 = 2048;

/// Largest image sent, in bytes before base64: Anthropic refuses one over
/// 5 MB once encoded.
pub const MAX_SEND_BYTES: usize = 3_750_000;

/// Largest file read at all: the attachment cap.
const MAX_READ_BYTES: u64 = crate::explorer::IMAGE_MAX_BYTES;

/// The types every dialect here takes.
const SENDABLE: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Fitted images kept.
const CACHE_SIZE: usize = 16;

/// Turns every [`WireImage::File`] in `request` into an inline image or a note.
pub async fn load(request: &mut ModelRequest) {
    for message in &mut request.messages {
        let images = match message {
            WireMessage::User { images, .. } | WireMessage::Tool { images, .. } => images,
            WireMessage::System { .. } | WireMessage::Assistant { .. } => continue,
        };
        for image in images.iter_mut() {
            let WireImage::File { path } = image else {
                continue;
            };
            let path = path.clone();
            let read = path.clone();
            *image = match tokio::task::spawn_blocking(move || fitted(&read)).await {
                Ok(Ok(fit)) => WireImage::Inline {
                    mime: fit.mime.to_owned(),
                    data: fit.data.clone(),
                },
                Ok(Err(reason)) => {
                    tracing::warn!(path = %path.display(), %reason, "an image was not sent");
                    WireImage::Missing { path, reason }
                }
                Err(_) => WireImage::Missing {
                    path,
                    reason: "it could not be read".to_owned(),
                },
            };
        }
    }
}

/// Replaces every image with a note: this dialect, in this build, carries
/// none. Said rather than dropped, so the model does not claim to see it.
pub fn refuse_all(request: &mut ModelRequest, reason: &str) {
    for message in &mut request.messages {
        if let WireMessage::User { images, .. } | WireMessage::Tool { images, .. } = message {
            for image in images.iter_mut() {
                let path = match image {
                    WireImage::File { path } | WireImage::Missing { path, .. } => path.clone(),
                    WireImage::Inline { .. } => PathBuf::new(),
                };
                *image = WireImage::Missing {
                    path,
                    reason: reason.to_owned(),
                };
            }
        }
    }
}

/// An image ready for a body.
#[derive(Debug)]
struct Fit {
    mime: &'static str,
    data: String,
}

type Key = (PathBuf, u64, Option<SystemTime>);

/// Fitted images, oldest first.
type Cache = Mutex<VecDeque<(Key, Arc<Fit>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(CACHE_SIZE)))
}

fn fitted(path: &Path) -> Result<Arc<Fit>, String> {
    let meta = std::fs::metadata(path).map_err(|_| "it is no longer on disk".to_owned())?;
    if !meta.is_file() {
        return Err("it is no longer on disk".to_owned());
    }
    if meta.len() > MAX_READ_BYTES {
        return Err("it is too large".to_owned());
    }
    let key: Key = (path.to_path_buf(), meta.len(), meta.modified().ok());

    {
        let held = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, fit)) = held.iter().find(|(held, _)| *held == key) {
            return Ok(Arc::clone(fit));
        }
    }

    let bytes = std::fs::read(path).map_err(|_| "it could not be read".to_owned())?;
    let fit = Arc::new(fit(&bytes)?);

    let mut held = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if held.len() >= CACHE_SIZE {
        held.pop_front();
    }
    held.push_back((key, Arc::clone(&fit)));
    Ok(fit)
}

/// The bytes as they will be sent: untouched when they already fit.
fn fit(bytes: &[u8]) -> Result<Fit, String> {
    let mime = crate::explorer::sniff(bytes)
        .filter(|mime| SENDABLE.contains(mime))
        .ok_or_else(|| "it is not a PNG, JPEG, GIF or WebP image".to_owned())?;

    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "it could not be read".to_owned())?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| "it could not be decoded".to_owned())?;

    if bytes.len() <= MAX_SEND_BYTES && width.max(height) <= MAX_SIDE {
        return Ok(Fit {
            mime,
            data: encode(bytes),
        });
    }

    let decoded =
        ::image::load_from_memory(bytes).map_err(|_| "it could not be decoded".to_owned())?;
    let scaled = if width.max(height) > MAX_SIDE {
        decoded.resize(MAX_SIDE, MAX_SIDE, ::image::imageops::FilterType::Triangle)
    } else {
        decoded
    };

    // PNG keeps text on a screen legible; JPEG is the fallback for a photo.
    let png = write(&scaled, ImageFormat::Png)?;
    if png.len() <= MAX_SEND_BYTES {
        return Ok(Fit {
            mime: "image/png",
            data: encode(&png),
        });
    }
    let jpeg = write(
        &DynamicImage::ImageRgb8(scaled.to_rgb8()),
        ImageFormat::Jpeg,
    )?;
    if jpeg.len() <= MAX_SEND_BYTES {
        return Ok(Fit {
            mime: "image/jpeg",
            data: encode(&jpeg),
        });
    }
    Err("it is too large to send, even downscaled".to_owned())
}

fn write(image: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), format)
        .map_err(|_| "it could not be re-encoded".to_owned())?;
    Ok(out)
}

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(::image::RgbaImage::new(width, height));
        write(&image, ImageFormat::Png).expect("encode")
    }

    fn request(images: Vec<WireImage>) -> ModelRequest {
        ModelRequest {
            model: "m".to_owned(),
            messages: vec![WireMessage::User {
                content: "look".to_owned(),
                images,
            }],
            tools: Vec::new(),
        }
    }

    fn images(request: &ModelRequest) -> &[WireImage] {
        match &request.messages[0] {
            WireMessage::User { images, .. } => images,
            other => panic!("expected a user message, got {other:?}"),
        }
    }

    #[test]
    fn a_small_image_is_sent_as_it_is() {
        let bytes = png(4, 3);
        let fit = fit(&bytes).expect("fits");
        assert_eq!(fit.mime, "image/png");
        assert_eq!(fit.data, encode(&bytes));
    }

    #[test]
    fn a_large_image_is_downscaled() {
        let fit = fit(&png(MAX_SIDE * 2, 10)).expect("fits");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(fit.data)
            .expect("base64");
        let (width, height) = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .expect("format")
            .into_dimensions()
            .expect("dimensions");
        assert_eq!(width, MAX_SIDE);
        assert_eq!(height, 5);
    }

    #[test]
    fn text_is_not_an_image() {
        assert!(fit(b"just words").is_err());
    }

    #[tokio::test]
    async fn files_are_read_and_a_missing_one_becomes_a_note() {
        let dir = TempDir::new().expect("dir");
        let here = dir.path().join("here.png");
        std::fs::write(&here, png(2, 2)).expect("write");
        let gone = dir.path().join("gone.png");

        let mut request = request(vec![
            WireImage::File { path: here },
            WireImage::File { path: gone.clone() },
        ]);
        load(&mut request).await;

        let images = images(&request);
        assert!(matches!(&images[0], WireImage::Inline { mime, .. } if mime == "image/png"));
        assert_eq!(
            images[1],
            WireImage::Missing {
                path: gone,
                reason: "it is no longer on disk".to_owned()
            }
        );
    }

    #[test]
    fn a_dialect_without_images_says_so() {
        let mut request = request(vec![WireImage::File {
            path: PathBuf::from("/x.png"),
        }]);
        refuse_all(&mut request, "this provider does not take images");
        assert!(matches!(
            &images(&request)[0],
            WireImage::Missing { reason, .. } if reason == "this provider does not take images"
        ));
    }
}
