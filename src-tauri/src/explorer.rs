//! The workspace explorer (PLAN 7.15): a read-only tree of the open project,
//! and a preview of one file in it.
//!
//! Read-only: there is no save path, since a WebView write would bypass the
//! gate (PLAN 7.6). Drops go through [`intake`](crate::intake).
//!
//! * **Contained**: every path goes through [`reveal::target`].
//! * **One directory at a time**, capped at [`LISTING_MAX_ENTRIES`], with the
//!   remainder counted.
//! * **Ignored is hidden, not unreachable**: `.git`, `node_modules` and
//!   ignore-file matches are hidden by default; `.aegis/` is shown; previews
//!   never check ignore rules.
//!
//! Text returns as a string and images as bytes for a blob URL — never a
//! `file://` URL.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::reveal;
use crate::workspace::{ARTEFACTS_DIR, BRIEFS_DIR, CABINET_DIR};
use crate::world::WORLD_DIR;

/// Most entries one listing returns; the rest are counted.
pub const LISTING_MAX_ENTRIES: usize = 1000;

/// Most bytes of a file a text preview carries; longer files say they were cut.
pub const TEXT_MAX_BYTES: u64 = 512 * 1024;

/// Largest image the window is handed as bytes (a binary IPC response).
pub const IMAGE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// How much of a file is looked at to decide whether it is text.
const SNIFF_BYTES: usize = 8 * 1024;

/// Names hidden wherever they appear, whatever the ignore files say.
///
/// Shown, marked, when ignored entries are requested.
const ALWAYS_IGNORED: [&str; 2] = [".git", "node_modules"];

/// Which part of the workspace a path is in, as far as a drop is concerned.
///
/// Lets the window mark the cabinet and pre-refuse drop targets; enforcement is
/// [`intake`](crate::intake)'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Zone {
    /// An ordinary part of the project.
    Plain,
    /// `.aegis/briefs/` and below: work going in, and where a drop lands.
    Briefs,
    /// `.aegis/artefacts/` and below: work coming out, written under the gate.
    Artefacts,
    /// The rest of `.aegis/`: status, decisions, the project's runbooks.
    Cabinet,
    /// `world/` and below: the constitution. Preview-only, like everything
    /// here, and never a drop target.
    World,
}

impl Zone {
    /// The zone of a workspace-relative path written with `/`.
    ///
    /// Only the first segment decides `world/`, matching the policy rule: a
    /// repository's own `src/world/` is an ordinary folder.
    pub fn of(rel: &str) -> Self {
        let rel = rel.trim_matches('/');
        let under = |dir: &str| {
            rel == dir
                || rel
                    .strip_prefix(dir)
                    .is_some_and(|rest| rest.starts_with('/'))
        };

        if under(WORLD_DIR) {
            Self::World
        } else if under(BRIEFS_DIR) {
            Self::Briefs
        } else if under(ARTEFACTS_DIR) {
            Self::Artefacts
        } else if under(CABINET_DIR) {
            Self::Cabinet
        } else {
            Self::Plain
        }
    }
}

/// What a row of the tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum EntryKind {
    /// A folder, or a link to one inside the workspace. Expandable.
    Dir,
    /// A file, or a link to one inside the workspace. Previewable.
    File,
    /// Anything else: a socket, a device, a dangling link.
    Other,
}

/// One row of the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TreeEntry {
    /// The entry's own name.
    pub name: String,
    /// Relative to the workspace root, with `/`. What the window sends back to
    /// expand or preview it.
    pub path: String,
    /// What it is.
    pub kind: EntryKind,
    /// A file's size. `None` for everything else.
    #[ts(type = "number | null")]
    pub bytes: Option<u64>,
    /// Whether it is hidden by default: `.git`, `node_modules`, or named by an
    /// ignore file — itself or a folder above it.
    pub ignored: bool,
    /// A link that lands outside the workspace. Listed so the folder is not
    /// misdescribed, and never expanded or previewed.
    pub outside: bool,
    /// Which part of the convention it is in.
    pub zone: Zone,
}

/// One folder of the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TreeListing {
    /// The folder, relative to the workspace root. Empty for the root.
    pub dir: String,
    /// Folders first, then files, by name.
    pub entries: Vec<TreeEntry>,
    /// Ignored entries left out of `entries`. Zero when they were asked for.
    pub hidden: u32,
    /// Entries past [`LISTING_MAX_ENTRIES`].
    pub more: u32,
}

/// What a preview shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum PreviewBody {
    /// Text, to be shown read-only. Markdown is rendered by the window as
    /// elements, never as HTML.
    Text {
        /// The first [`TEXT_MAX_BYTES`] of the file, decoded.
        text: String,
        /// Whether the file is longer than `text`.
        truncated: bool,
        /// Whether the name says it is markdown.
        markdown: bool,
    },
    /// An image the window can fetch as bytes with `workspace_image`.
    Image {
        /// The type its first bytes say it is.
        mime: String,
    },
    /// Neither. Name, size and type are all a preview says about it.
    Binary {
        /// The type its first bytes say it is, when they say.
        mime: Option<String>,
    },
}

/// One file, as the preview pane draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct FilePreview {
    /// Relative to the workspace root, with `/`.
    pub path: String,
    /// The file's own name.
    pub name: String,
    /// Its size on disk.
    #[ts(type = "number")]
    pub bytes: u64,
    /// When it last changed, RFC3339 UTC, when the filesystem says.
    pub modified: Option<String>,
    /// Which part of the convention it is in.
    pub zone: Zone,
    /// What there is to show.
    pub body: PreviewBody,
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// One folder of the workspace, or the root when `dir` is empty.
///
/// `show_ignored` includes hidden entries, marked — a view toggle, not a
/// permission.
pub fn list(root: &Path, dir: Option<&str>, show_ignored: bool) -> AppResult<TreeListing> {
    let rel_dir = normalize(dir.unwrap_or(""));
    let target = reveal::target(root, Some(&rel_dir))?;
    if !target.is_dir() {
        return Err(AppError::RevealPath {
            path: rel_dir,
            reason: "it is not a folder".to_owned(),
        });
    }

    // Only worth the walk when ignored rows are being drawn: otherwise an
    // ignored folder is never shown, so nobody expands one.
    let parent_ignored = show_ignored && inside_ignored(root, &rel_dir);
    let visible = visible_names(&target);
    let read = fs::read_dir(&target).map_err(|err| unreadable(&rel_dir, &err))?;

    let mut entries = Vec::new();
    let mut hidden: u32 = 0;
    for entry in read.filter_map(Result::ok) {
        let os_name = entry.file_name();
        let name = os_name.to_string_lossy().into_owned();
        let ignored = parent_ignored
            || ALWAYS_IGNORED.contains(&name.as_str())
            || !visible.contains(&os_name);

        if ignored && !show_ignored {
            hidden = hidden.saturating_add(1);
            continue;
        }

        let path = join(&rel_dir, &name);
        let (kind, bytes, outside) = describe(root, &entry, &path);
        entries.push(TreeEntry {
            zone: Zone::of(&path),
            name,
            path,
            kind,
            bytes,
            ignored,
            outside,
        });
    }

    entries.sort_by(|left, right| {
        (left.kind != EntryKind::Dir)
            .cmp(&(right.kind != EntryKind::Dir))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });

    let total = entries.len();
    entries.truncate(LISTING_MAX_ENTRIES);
    Ok(TreeListing {
        dir: rel_dir,
        more: u32::try_from(total - entries.len()).unwrap_or(u32::MAX),
        hidden,
        entries,
    })
}

/// The names directly inside `dir` that the ignore files leave visible.
///
/// Uses the `ignore` crate to match git exactly, inside a work tree only.
fn visible_names(dir: &Path) -> HashSet<OsString> {
    let mut builder = ignore::WalkBuilder::new(dir);
    builder
        .max_depth(Some(1))
        .hidden(false)
        .parents(true)
        .ignore(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(true)
        .follow_links(false);

    let mut names = HashSet::new();
    for item in builder.build() {
        match item {
            Ok(entry) if entry.depth() == 1 => {
                names.insert(entry.file_name().to_owned());
            }
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(%err, dir = %dir.display(), "an ignore rule could not be applied");
            }
        }
    }
    names
}

/// Whether some folder on the way down to `rel_dir` is itself ignored.
///
/// Checked per ancestor, since an ignored folder's children match no rule
/// themselves.
fn inside_ignored(root: &Path, rel_dir: &str) -> bool {
    let mut at = root.to_path_buf();
    for segment in rel_dir.split('/').filter(|s| !s.is_empty() && *s != ".") {
        if ALWAYS_IGNORED.contains(&segment) || !visible_names(&at).contains(OsStr::new(segment)) {
            return true;
        }
        at.push(segment);
    }
    false
}

/// What one directory entry is, judged where it lands.
fn describe(root: &Path, entry: &fs::DirEntry, rel: &str) -> (EntryKind, Option<u64>, bool) {
    let Ok(file_type) = entry.file_type() else {
        return (EntryKind::Other, None, false);
    };

    if file_type.is_symlink() {
        // Through the same door as a preview, so a link that climbs out is
        // drawn as what it is rather than as a folder that cannot be opened.
        let Ok(target) = reveal::target(root, Some(rel)) else {
            return (EntryKind::Other, None, true);
        };
        return match fs::metadata(&target) {
            Ok(meta) if meta.is_dir() => (EntryKind::Dir, None, false),
            Ok(meta) if meta.is_file() => (EntryKind::File, Some(meta.len()), false),
            _ => (EntryKind::Other, None, false),
        };
    }

    if file_type.is_dir() {
        (EntryKind::Dir, None, false)
    } else if file_type.is_file() {
        (
            EntryKind::File,
            entry.metadata().ok().map(|meta| meta.len()),
            false,
        )
    } else {
        (EntryKind::Other, None, false)
    }
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// One file of the workspace, as something to show.
///
/// A folder, a missing file and a path outside the workspace are refused —
/// the last as [`AppError::RevealOutside`], the same answer reveal gives.
pub fn preview(root: &Path, raw: &str) -> AppResult<FilePreview> {
    let rel = normalize(raw);
    let file = contained_file(root, &rel)?;
    let meta = fs::metadata(&file).map_err(|err| unreadable(&rel, &err))?;
    let head = read_head(&file, TEXT_MAX_BYTES).map_err(|err| unreadable(&rel, &err))?;

    let body = match sniff(&head) {
        Some(mime) if mime.starts_with("image/") && meta.len() <= IMAGE_MAX_BYTES => {
            PreviewBody::Image {
                mime: mime.to_owned(),
            }
        }
        Some(mime) => PreviewBody::Binary {
            mime: Some(mime.to_owned()),
        },
        None => match as_text(&head) {
            Some(text) => PreviewBody::Text {
                text,
                truncated: meta.len() > TEXT_MAX_BYTES,
                markdown: is_markdown(&rel),
            },
            None => PreviewBody::Binary { mime: None },
        },
    };

    Ok(FilePreview {
        name: rel.rsplit('/').next().unwrap_or(&rel).to_owned(),
        bytes: meta.len(),
        modified: meta
            .modified()
            .ok()
            .map(|at| DateTime::<Utc>::from(at).to_rfc3339_opts(SecondsFormat::Secs, true)),
        zone: Zone::of(&rel),
        path: rel,
        body,
    })
}

/// The bytes of one image, and the type they are.
///
/// Refuses anything whose first bytes are not an image this window draws — a
/// name ending `.png` is not evidence — and anything over [`IMAGE_MAX_BYTES`].
/// SVG is deliberately not here: it is markup, and it is previewed as text.
pub fn image(root: &Path, raw: &str) -> AppResult<(Vec<u8>, &'static str)> {
    let rel = normalize(raw);
    let file = contained_file(root, &rel)?;
    let meta = fs::metadata(&file).map_err(|err| unreadable(&rel, &err))?;
    if meta.len() > IMAGE_MAX_BYTES {
        return Err(AppError::RevealPath {
            path: rel,
            reason: "it is too large to preview".to_owned(),
        });
    }

    let bytes = read_head(&file, IMAGE_MAX_BYTES).map_err(|err| unreadable(&rel, &err))?;
    match sniff(&bytes) {
        Some(mime) if mime.starts_with("image/") => Ok((bytes, mime)),
        _ => Err(AppError::RevealPath {
            path: rel,
            reason: "it is not an image this window can show".to_owned(),
        }),
    }
}

/// A contained, existing file, or why not.
fn contained_file(root: &Path, rel: &str) -> AppResult<PathBuf> {
    if rel.is_empty() {
        return Err(AppError::RevealPath {
            path: String::new(),
            reason: "no file was named".to_owned(),
        });
    }

    let path = reveal::target(root, Some(rel))?;
    if path.is_file() {
        return Ok(path);
    }
    Err(AppError::RevealPath {
        path: rel.to_owned(),
        reason: if path.is_dir() {
            "it is a folder"
        } else {
            "it is not there any more"
        }
        .to_owned(),
    })
}

/// At most `cap` bytes from the start of a file.
fn read_head(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(cap).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The type a file's first bytes declare, for the few types a preview treats
/// differently from text.
pub(crate) fn sniff(head: &[u8]) -> Option<&'static str> {
    const SIGNATURES: [(&[u8], &str); 8] = [
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xFF\xD8\xFF", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"\x00\x00\x01\x00", "image/x-icon"),
        (b"%PDF-", "application/pdf"),
        (b"PK\x03\x04", "application/zip"),
        (b"\x1F\x8B", "application/gzip"),
    ];

    if let Some((_, mime)) = SIGNATURES
        .iter()
        .find(|(signature, _)| head.starts_with(signature))
    {
        return Some(mime);
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    // Two letters are a word as often as they are a bitmap, so the header's
    // reserved bytes have to be zero as well.
    if head.len() >= 26 && head.starts_with(b"BM") && head[6..10] == [0, 0, 0, 0] {
        return Some("image/bmp");
    }
    None
}

/// The bytes as text, or `None` when they are not text.
///
/// A NUL in the first kilobytes means binary. Non-UTF-8 text is shown lossily
/// unless replacement characters dominate.
fn as_text(head: &[u8]) -> Option<String> {
    if head[..head.len().min(SNIFF_BYTES)].contains(&0) {
        return None;
    }

    let text = match std::str::from_utf8(head) {
        Ok(text) => text.to_owned(),
        // The cap cut the last character in half; the file is still text.
        Err(err) if err.error_len().is_none() => {
            String::from_utf8_lossy(&head[..err.valid_up_to()]).into_owned()
        }
        Err(_) => {
            let lossy = String::from_utf8_lossy(head);
            let total = lossy.chars().count().max(1);
            let replaced = lossy
                .chars()
                .filter(|c| *c == char::REPLACEMENT_CHARACTER)
                .count();
            if replaced * 10 > total {
                return None;
            }
            lossy.into_owned()
        }
    };

    Some(
        text.strip_prefix('\u{FEFF}')
            .map(str::to_owned)
            .unwrap_or(text),
    )
}

/// Whether the name says markdown.
fn is_markdown(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// A path from the window, in the one spelling rows are keyed by.
///
/// `/` separators, no leading `./` or trailing `/`. Containment is
/// [`reveal::target`]'s.
fn normalize(raw: &str) -> String {
    let mut rel = raw.trim().replace('\\', "/");
    while let Some(rest) = rel.strip_prefix("./") {
        rel = rest.to_owned();
    }
    let trimmed = rel.trim_end_matches('/');
    if trimmed == "." {
        String::new()
    } else {
        trimmed.to_owned()
    }
}

/// `dir/name`, or `name` at the root.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// An `io::Error` about somebody's file, as something they can act on.
fn unreadable(rel: &str, err: &io::Error) -> AppError {
    tracing::debug!(%err, rel, "a workspace path could not be read for the explorer");
    AppError::RevealPath {
        path: rel.to_owned(),
        reason: match err.kind() {
            io::ErrorKind::NotFound => "it is not there any more",
            io::ErrorKind::PermissionDenied => "it is not readable",
            _ => "it could not be read",
        }
        .to_owned(),
    }
}

#[cfg(test)]
mod tests;
