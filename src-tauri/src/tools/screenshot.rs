//! The screen capture tool: `screen_capture`.
//!
//! The largest blast radius in the MVP (PLAN 5.4): a capture is whatever is on
//! screen.
//!
//! * Always asked (PLAN 3).
//! * The PNG is written outside the workspace and only its path crosses IPC;
//!   the WebView reads it through the capture-scoped asset protocol.
//! * The audit line holds path, dimensions and SHA-256 — never the image.
//!
//! Platform failures (PLAN 5.1–5.3):
//!
//! * **macOS** TCC denial returns a blank frame, so a uniformly coloured frame
//!   is `E_SCREEN_PERMISSION`.
//! * **Wayland** may refuse; a failure there is `E_SCREEN_PERMISSION` naming the
//!   session type.
//! * **Windows** captures the physical framebuffer; the dialog shows physical
//!   and logical sizes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use xcap::image::codecs::png::PngEncoder;
use xcap::image::{ExtendedColorType, ImageEncoder, RgbaImage};
use xcap::Monitor;

use super::Produced;
use crate::audit::AuditArtifact;
use crate::error::ErrorCode;
use crate::policy::{tool, ScreenGeometry};

/// The only display selector this build accepts (PLAN 4.1).
///
/// Only the primary display: a prompt a user can check against their screen.
pub const PRIMARY: &str = "primary";

/// What the dialog calls the display; a real model name is appended when known.
const PRIMARY_LABEL: &str = "the primary display";

/// The placeholder `xcap` returns on Windows for a monitor whose name is not in
/// the registry — `Unknown Monitor 65537` and the like.
///
/// Left out of the prompt.
const PLACEHOLDER_NAME: &str = "unknown monitor";

/// JSON Schema for `screen_capture` arguments (PLAN 4.1).
pub fn capture_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "display": {
                "type": "string",
                "enum": [PRIMARY],
                "description": "Which display to capture. Only the primary display can be \
                                captured, and the argument may be omitted.",
            },
        },
        "required": [],
        "additionalProperties": false,
    })
}

/// Measures the display a capture would take, for the approval dialog.
///
/// Reads geometry only, never pixels, so it is safe before approval. `None`
/// without a display.
pub fn geometry() -> Option<ScreenGeometry> {
    let monitor = primary().ok()?;

    let width = monitor.width().ok()?;
    let height = monitor.height().ok()?;
    // A scale factor the platform will not report, or reports as zero, means
    // "no scaling as far as anything here can tell" — which is a better answer
    // than dividing by it.
    let scale = monitor
        .scale_factor()
        .ok()
        .filter(|factor| *factor > 0.0)
        .unwrap_or(1.0);

    Some(ScreenGeometry {
        display: name_of(&monitor),
        width,
        height,
        logical_width: logical(width, scale),
        logical_height: logical(height, scale),
    })
}

/// Captures the primary display and writes it to `dir` as a PNG.
///
/// The envelope carries path, dimensions and digest — never the bytes
/// (PLAN 4.3).
pub(crate) fn capture(display: &str, dir: &Path) -> Produced {
    // Policy already rejected anything else, so this is a second reading of a
    // decision rather than a first one. It stays because the two modules are
    // separately testable, and a tool that trusts its caller to have checked is
    // a tool that stops being true the day a second caller appears.
    if !display.eq_ignore_ascii_case(PRIMARY) {
        return Produced::failed(
            tool::SCREEN_CAPTURE,
            ErrorCode::ToolFailed,
            format!("`{display}` is not a display this build can capture"),
        );
    }

    let monitor = match primary() {
        Ok(monitor) => monitor,
        Err(message) => return unavailable(&message),
    };
    let label = name_of(&monitor);

    let image = match monitor.capture_image() {
        Ok(image) => image,
        Err(err) => {
            tracing::debug!(%err, "the window server refused a capture");
            return unavailable(&err.to_string());
        }
    };

    // The TCC path (PLAN 5.2). A capture the OS refused arrives as a frame
    // rather than as an error, and handing the model a black PNG would let it
    // reason about an empty screen as though it had seen one.
    if is_uniform(&image) {
        return Produced::failed(
            tool::SCREEN_CAPTURE,
            ErrorCode::ScreenPermission,
            blank_reason(&label),
        );
    }

    let capture = match save(dir, &image) {
        Ok(capture) => capture,
        Err(err) => {
            tracing::warn!(%err, dir = %dir.display(), "a capture could not be written");
            return Produced::failed(
                tool::SCREEN_CAPTURE,
                ErrorCode::ToolFailed,
                format!("the capture could not be written to `{}`", dir.display()),
            );
        }
    };

    let shown = capture.path.display();
    let summary = format!(
        "captured {label} to {shown} ({} x {})",
        capture.width, capture.height
    );

    Produced::ok(
        tool::SCREEN_CAPTURE,
        summary,
        format!(
            "Captured {label}: {} x {} pixels, written to {shown}. The picture itself is not \
             in this result; it follows it.",
            capture.width, capture.height
        ),
        capture.bytes,
        false,
        json!({
            "path": shown.to_string(),
            "width": capture.width,
            "height": capture.height,
            "sha256": capture.sha256,
        }),
    )
    .with_artifact(capture.artifact())
}

/// A PNG on disk, and everything said about it afterwards.
struct Capture {
    /// Where it was written.
    path: PathBuf,
    /// Its width in physical pixels.
    width: u32,
    /// Its height in physical pixels.
    height: u32,
    /// The size of the encoded file.
    bytes: u64,
    /// SHA-256 of the file, hex.
    sha256: String,
}

impl Capture {
    /// What the audit log records about it (PLAN 5.4).
    fn artifact(&self) -> AuditArtifact {
        AuditArtifact {
            path: self.path.display().to_string(),
            sha256: self.sha256.clone(),
            width: self.width,
            height: self.height,
        }
    }
}

/// Encodes a frame as a PNG under `dir` and describes what was written.
///
/// The digest covers the encoded PNG, so the file on disk can be checked.
fn save(dir: &Path, image: &RgbaImage) -> io::Result<Capture> {
    fs::create_dir_all(dir)?;

    let (width, height) = image.dimensions();
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(image.as_raw(), width, height, ExtendedColorType::Rgba8)
        .map_err(io::Error::other)?;

    let path = dir.join(filename());
    fs::write(&path, &png)?;

    Ok(Capture {
        path,
        width,
        height,
        bytes: png.len() as u64,
        sha256: hex(&Sha256::digest(&png)),
    })
}

/// A name for one capture: sortable, unique, and readable in a folder.
///
/// The timestamp is what makes a directory listing chronological; the random
/// suffix is what stops two captures in the same second from becoming one file.
fn filename() -> String {
    let stamp = Utc::now()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
        .replace([':', '-'], "")
        .replace('Z', "");
    let unique = uuid::Uuid::new_v4().simple().to_string();

    format!("capture-{stamp}-{}.png", &unique[..8])
}

/// Lowercase hex of a digest.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::new(), |mut out, byte| {
        // Writing into a `String` cannot fail; the result is discarded rather
        // than unwrapped so this stays free of a panic path (AGENTS.md).
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The primary display, or why there is not one.
///
/// Falls back to the first display when none is marked primary.
fn primary() -> Result<Monitor, String> {
    let monitors = Monitor::all().map_err(|err| err.to_string())?;

    monitors
        .iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .cloned()
        .ok_or_else(|| "this machine reports no display".to_owned())
}

/// How the dialog and the transcript should name a display.
fn name_of(monitor: &Monitor) -> String {
    let model = monitor
        .friendly_name()
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && !name.to_lowercase().starts_with(PLACEHOLDER_NAME));

    model.map_or_else(
        || PRIMARY_LABEL.to_owned(),
        |name| format!("{PRIMARY_LABEL} — {name}"),
    )
}

/// The logical size of a physical dimension at a given scale.
///
/// Rounded, not truncated (2560 at 1.5 → 1707).
fn logical(physical: u32, scale: f32) -> u32 {
    #[allow(clippy::cast_precision_loss)]
    let value = (physical as f32 / scale).round();

    // A scale the platform reported nonsensically means a logical size that
    // does not fit, or is not a number at all. The physical size is the honest
    // fallback: it is at least a measurement.
    if value.is_finite() && (1.0..=f32::from(u16::MAX)).contains(&value) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            value as u32
        }
    } else {
        physical
    }
}

/// Whether every pixel of a frame is the same colour.
///
/// The macOS TCC denial signal (PLAN 5.2). Exact uniformity, not darkness: a
/// false permission error is cheaper than a model reading a blank frame.
fn is_uniform(image: &RgbaImage) -> bool {
    let raw = image.as_raw();
    let Some(first) = raw.get(..4) else {
        // No pixels at all. Not a picture of anything either.
        return true;
    };

    raw.chunks_exact(4).all(|pixel| pixel == first)
}

/// The envelope for a capture the platform would not perform.
///
/// Always `E_SCREEN_PERMISSION`; the message carries the specific reason.
fn unavailable(detail: &str) -> Produced {
    let message = if is_wayland() {
        format!(
            "this is a Wayland session, which does not let an application read the screen \
             directly ({detail}). Log in to an X11 session, or use a compositor that answers \
             screencopy requests."
        )
    } else if cfg!(target_os = "macos") {
        format!("{} ({detail})", macos_hint())
    } else {
        format!("the screen could not be captured: {detail}")
    };

    Produced::failed(tool::SCREEN_CAPTURE, ErrorCode::ScreenPermission, message)
}

/// The message for a capture that succeeded and came back blank.
fn blank_reason(label: &str) -> String {
    if is_wayland() {
        format!(
            "{label} came back blank, which under Wayland means the compositor did not allow the \
             capture. Nothing was written."
        )
    } else if cfg!(target_os = "macos") {
        format!("{label} came back blank. {}", macos_hint())
    } else {
        format!(
            "{label} came back blank — every pixel the same colour — so nothing was written. The \
             display may be asleep or locked."
        )
    }
}

/// The one sentence a macOS user needs.
fn macos_hint() -> &'static str {
    "macOS has not granted Aegis Screen Recording: allow it under System Settings → Privacy & \
     Security → Screen Recording, then try again. Under `pnpm tauri dev` the consent belongs to \
     the dev binary and has to be granted again after that binary is rebuilt"
}

/// Whether this process is talking to a Wayland compositor.
///
/// From `XDG_SESSION_TYPE`, else `WAYLAND_DISPLAY`.
fn is_wayland() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }

    std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind.eq_ignore_ascii_case("wayland"))
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;
    use xcap::image::Rgba;

    /// A frame with more than one colour in it, so nothing mistakes it for a
    /// refused capture.
    fn frame(width: u32, height: u32) -> RgbaImage {
        let mut image = RgbaImage::new(width, height);
        for (index, pixel) in image.pixels_mut().enumerate() {
            let shade = u8::try_from(index % 256).unwrap_or(0);
            *pixel = Rgba([shade, 16, 32, 255]);
        }
        image
    }

    #[test]
    fn a_frame_is_written_as_a_png_and_described_by_its_digest() {
        let dir = TempDir::new().expect("temp dir");
        let capture = save(dir.path(), &frame(8, 4)).expect("written");

        assert_eq!((capture.width, capture.height), (8, 4));
        assert!(capture.path.starts_with(dir.path()));
        assert_eq!(
            capture.path.extension().and_then(|ext| ext.to_str()),
            Some("png")
        );

        let bytes = fs::read(&capture.path).expect("readable");
        assert_eq!(bytes.len() as u64, capture.bytes);
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a real PNG header");
        assert_eq!(capture.sha256, hex(&Sha256::digest(&bytes)));
        assert_eq!(capture.sha256.len(), 64);
    }

    /// The audit line names the file and proves which one it was, and holds
    /// nothing of what was on the screen (PLAN 5.4).
    #[test]
    fn the_audit_artifact_identifies_the_file_and_nothing_else() {
        let dir = TempDir::new().expect("temp dir");
        let capture = save(dir.path(), &frame(6, 6)).expect("written");
        let artifact = capture.artifact();

        assert_eq!(artifact.path, capture.path.display().to_string());
        assert_eq!(artifact.sha256, capture.sha256);
        assert_eq!((artifact.width, artifact.height), (6, 6));

        let rendered = serde_json::to_string(&artifact).expect("serializes");
        assert!(!rendered.contains("data:"), "{rendered}");
        assert!(rendered.len() < 512, "an identifier, not a copy");
    }

    #[test]
    fn the_capture_directory_is_created_on_first_use() {
        let dir = TempDir::new().expect("temp dir");
        let nested = dir.path().join("captures");

        assert!(!nested.exists());
        let capture = save(&nested, &frame(2, 2)).expect("written");
        assert!(capture.path.is_file());
    }

    #[test]
    fn two_captures_in_the_same_second_are_two_files() {
        let dir = TempDir::new().expect("temp dir");
        let first = save(dir.path(), &frame(2, 2)).expect("written");
        let second = save(dir.path(), &frame(2, 2)).expect("written");

        assert_ne!(first.path, second.path);
        assert_eq!(fs::read_dir(dir.path()).expect("listing").count(), 2);
    }

    /// The macOS refusal path (PLAN 5.2): the API succeeds and the frame is
    /// blank. A frame with anything at all in it is not that.
    #[test]
    fn a_blank_frame_is_recognized_and_a_real_one_is_not() {
        let black = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 255]));
        assert!(is_uniform(&black), "an all-black frame");

        let flat = RgbaImage::from_pixel(4, 4, Rgba([200, 200, 200, 255]));
        assert!(is_uniform(&flat), "uniform, not merely dark");

        assert!(!is_uniform(&frame(4, 4)), "a frame with content in it");
    }

    #[test]
    fn a_display_that_cannot_be_captured_is_a_permission_problem() {
        let produced = unavailable("the window server said no");

        assert!(!produced.result.ok);
        assert_eq!(
            produced.result.error.map(|error| error.code),
            Some(ErrorCode::ScreenPermission)
        );
    }

    /// Reached only if policy ever stopped filtering; it still must not write
    /// anything.
    #[test]
    fn only_the_primary_display_can_be_asked_for() {
        let dir = TempDir::new().expect("temp dir");
        let produced = capture("hdmi-2", &dir.path().join("captures"));

        assert!(!produced.result.ok);
        assert_eq!(
            produced.result.error.map(|error| error.code),
            Some(ErrorCode::ToolFailed)
        );
        assert!(!dir.path().join("captures").exists(), "nothing was written");
    }

    #[test]
    fn the_schema_offers_the_one_display_this_build_captures() {
        let schema = capture_schema();

        assert_eq!(schema["properties"]["display"]["enum"], json!(["primary"]));
        assert_eq!(schema["required"], json!([]));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn a_logical_size_is_the_physical_one_divided_by_the_scale() {
        assert_eq!(logical(2560, 1.0), 2560);
        assert_eq!(logical(2560, 1.5), 1707);
        assert_eq!(logical(3840, 2.0), 1920);
        assert_eq!(logical(1920, 0.0), 1920, "a scale of zero is not a scale");
    }
}
