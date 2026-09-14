//! The filesystem tools: `fs_list`, `fs_read`, `fs_write`.
//!
//! Paths arrive already resolved and judged by [`policy`](crate::policy); this
//! module must never re-derive one.
//!
//! * **Caps** (PLAN 4.3): `fs_read` ≤ 256 KB, `fs_list` ≤ 1000 entries; callers
//!   may ask for less, and truncation is stated.
//! * **Text means UTF-8**: invalid UTF-8 is reported, never lossily converted.
//! * **Failures are envelopes** (`ok: false`); the turn continues.

use std::fs;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use serde_json::{json, Value};

use super::{Produced, LIST_MAX_ENTRIES, READ_MAX_BYTES};
use crate::error::ErrorCode;
use crate::policy::tool;

/// One entry of a listing, before it is rendered.
///
/// Sorted directories first, then by name, so listings are stable.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    /// `false` sorts before `true`, so this reads backwards on purpose:
    /// directories first.
    not_dir: bool,
    /// The entry's name, as it is on disk.
    name: String,
    /// Its size in bytes, for a regular file.
    size: Option<u64>,
    /// Whether the entry is itself a symbolic link.
    link: bool,
}

impl Entry {
    /// One line of the listing.
    ///
    /// `dir/`, `link@`, or `file<TAB>size`; no padding.
    fn render(&self) -> String {
        let mark = if self.not_dir { "" } else { "/" };
        let link = if self.link { "@" } else { "" };
        match self.size {
            Some(size) if self.not_dir => format!("{}{link}\t{size}", self.name),
            _ => format!("{}{mark}{link}", self.name),
        }
    }
}

// ---------------------------------------------------------------------------
// fs_list
// ---------------------------------------------------------------------------

/// JSON Schema for `fs_list` arguments (PLAN 4.1).
pub fn list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Directory to list. Relative paths are resolved against the \
                                workspace root.",
            },
            "max_entries": {
                "type": "integer",
                "minimum": 1,
                "maximum": LIST_MAX_ENTRIES,
                "description": "Most entries to return. Capped at 1000 regardless.",
            },
        },
        "required": ["path"],
        "additionalProperties": false,
    })
}

/// Lists a directory.
///
/// `bytes` is the rendered length; `meta.total_entries` counts past the cap.
pub(crate) fn list(path: &Path, max_entries: Option<u32>) -> Produced {
    let cap = max_entries
        .unwrap_or(LIST_MAX_ENTRIES)
        .min(LIST_MAX_ENTRIES) as usize;

    let reader = match fs::read_dir(path) {
        Ok(reader) => reader,
        Err(err) => return failed_io(tool::FS_LIST, path, &err, "list"),
    };

    let mut entries: Vec<Entry> = Vec::new();
    let mut total = 0usize;

    for entry in reader {
        // One unreadable entry is not a reason to fail the listing: a file
        // that vanished between `readdir` and `stat` is ordinary on a live
        // workspace, and the other entries are still the answer.
        let Ok(entry) = entry else { continue };
        total += 1;

        // `entry.file_type` does not follow links, which is what is wanted
        // here: the listing describes what is *in* this directory, and a link
        // is a thing in it. Whether following it would leave the workspace is
        // policy's question, asked when something tries to open it.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let size = entry.metadata().ok().map(|meta| meta.len());

        entries.push(Entry {
            not_dir: !kind.is_dir(),
            name,
            size,
            link: kind.is_symlink(),
        });
    }

    entries.sort();
    entries.truncate(cap);
    let shown = entries.len();

    // Measured against what the directory holds, not against the cap: an entry
    // the filesystem would not describe was skipped above and is missing from
    // the answer just as surely as one the cap cut off. Either way the model is
    // told it is not seeing everything.
    let truncated = shown < total;

    let mut content = entries
        .iter()
        .map(Entry::render)
        .collect::<Vec<_>>()
        .join("\n");

    if truncated {
        // Said in the content as well as in the envelope: a model that only
        // reads the text still learns it is looking at part of a directory.
        content.push_str(&format!(
            "\n… {} more entries not shown",
            total.saturating_sub(shown)
        ));
    }

    let summary = if truncated {
        format!("listed {shown} of {total} entries in {}", path.display())
    } else {
        format!("listed {shown} entries in {}", path.display())
    };

    Produced::ok(
        tool::FS_LIST,
        summary,
        content.clone(),
        content.len() as u64,
        truncated,
        json!({
            "path": path.display().to_string(),
            "entries": shown,
            "total_entries": total,
        }),
    )
}

// ---------------------------------------------------------------------------
// fs_read
// ---------------------------------------------------------------------------

/// JSON Schema for `fs_read` arguments (PLAN 4.1).
pub fn read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "File to read. Relative paths are resolved against the workspace \
                                root.",
            },
            "offset": {
                "type": "integer",
                "minimum": 0,
                "description": "Byte offset to start at. Defaults to the start of the file.",
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": READ_MAX_BYTES,
                "description": "Most bytes to return. Capped at 262144 regardless.",
            },
        },
        "required": ["path"],
        "additionalProperties": false,
    })
}

/// Reads a window of a text file.
///
/// `bytes` on the envelope is the file's whole size, so a model that reads the
/// first 256 KB of a 4 MB file can see what it is missing and ask for the next
/// window by `offset`.
pub(crate) fn read(path: &Path, offset: Option<u64>, limit: Option<u64>) -> Produced {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) => return failed_io(tool::FS_READ, path, &err, "read"),
    };
    if meta.is_dir() {
        return Produced::failed(
            tool::FS_READ,
            ErrorCode::ToolFailed,
            format!("`{}` is a directory; use fs_list", path.display()),
        );
    }

    let total = meta.len();
    let offset = offset.unwrap_or(0);
    let want = limit.unwrap_or(READ_MAX_BYTES).min(READ_MAX_BYTES);

    if offset >= total && total > 0 {
        return Produced::failed(
            tool::FS_READ,
            ErrorCode::ToolFailed,
            format!("offset {offset} is past the end of a {total}-byte file"),
        );
    }

    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) => return failed_io(tool::FS_READ, path, &err, "read"),
    };
    if offset > 0 {
        if let Err(err) = file.seek(SeekFrom::Start(offset)) {
            return failed_io(tool::FS_READ, path, &err, "read");
        }
    }

    // One extra byte beyond the cap, so "did the window reach the end?" is
    // answered by what was read rather than by trusting the size the metadata
    // reported for a file another process may be appending to.
    let mut bytes = Vec::new();
    if let Err(err) = file.take(want + 1).read_to_end(&mut bytes) {
        return failed_io(tool::FS_READ, path, &err, "read");
    }

    let over_cap = bytes.len() as u64 > want;
    bytes.truncate(want as usize);

    let Some(text) = text_window(&bytes, offset > 0) else {
        return Produced::failed(
            tool::FS_READ,
            ErrorCode::ToolFailed,
            format!(
                "`{}` is not UTF-8 text ({total} bytes); this build reads text files only",
                path.display()
            ),
        );
    };

    // Two different reasons the answer can be short: the cap, and a partial
    // character trimmed off the end of the window. Either way the model is
    // told, so it never treats a cut window as a whole file.
    let truncated = over_cap || (text.len() as u64) < bytes.len() as u64;
    let summary = format!(
        "read {} bytes from {}{}",
        text.len(),
        path.display(),
        if truncated { " (truncated)" } else { "" }
    );

    Produced::ok(
        tool::FS_READ,
        summary,
        text,
        total,
        truncated,
        json!({
            "path": path.display().to_string(),
            "offset": offset,
            "bytes_total": total,
        }),
    )
}

/// Interprets a byte window as text, or reports that it is not text.
///
/// Tolerates a character split at either edge: leading continuation bytes (only
/// with a non-zero offset) and a trailing incomplete sequence are trimmed. Any
/// other invalid sequence means `None`.
fn text_window(bytes: &[u8], trim_leading: bool) -> Option<String> {
    let start = if trim_leading {
        // A UTF-8 continuation byte is `10xxxxxx`; at most three can precede
        // the start of a character.
        bytes
            .iter()
            .take(3)
            .take_while(|byte| (*byte & 0xC0) == 0x80)
            .count()
    } else {
        0
    };
    let bytes = &bytes[start..];

    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text.to_owned()),
        Err(err) if err.error_len().is_none() => {
            // Incomplete sequence at the end: the file is text, the window
            // just stopped mid-character.
            std::str::from_utf8(&bytes[..err.valid_up_to()])
                .ok()
                .map(str::to_owned)
        }
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// fs_write
// ---------------------------------------------------------------------------

/// JSON Schema for `fs_write` arguments (PLAN 4.1).
pub fn write_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "File to write. Relative paths are resolved against the workspace \
                                root.",
            },
            "content": {
                "type": "string",
                "description": "The whole new content of the file. Written exactly, with nothing \
                                added.",
            },
            "create_dirs": {
                "type": "boolean",
                "description": "Create missing parent directories. Defaults to false.",
            },
        },
        "required": ["path", "content"],
        "additionalProperties": false,
    })
}

/// Writes a file, replacing whatever was there.
///
/// A plain write, not temp-and-rename, which would break hard links and editor
/// watches in the user's workspace.
pub(crate) fn write(path: &Path, content: &str, create_dirs: bool) -> Produced {
    let existed = path.exists();

    if create_dirs {
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                return failed_io(tool::FS_WRITE, parent, &err, "create");
            }
        }
    }

    let bytes = content.len() as u64;
    if let Err(err) = fs::write(path, content) {
        return failed_io(tool::FS_WRITE, path, &err, "write");
    }

    let verb = if existed { "replaced" } else { "created" };
    let summary = format!("{verb} {} ({bytes} bytes)", path.display());

    Produced::ok(
        tool::FS_WRITE,
        summary.clone(),
        summary,
        bytes,
        false,
        json!({
            "path": path.display().to_string(),
            "bytes": bytes,
            "created": !existed,
        }),
    )
    .with_bytes_in(bytes)
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/// Turns an `io::Error` into an envelope the model can act on.
///
/// Names the path and action; the platform-specific `io::Error` goes to the log.
fn failed_io(tool_name: &str, path: &Path, err: &io::Error, action: &str) -> Produced {
    tracing::debug!(%err, path = %path.display(), tool = tool_name, "a tool call failed");

    let shown = path.display();
    let message = match err.kind() {
        io::ErrorKind::NotFound => format!("`{shown}` does not exist"),
        io::ErrorKind::PermissionDenied => format!("`{shown}` is not readable or writable by you"),
        io::ErrorKind::NotADirectory => format!("`{shown}` is not a directory"),
        io::ErrorKind::IsADirectory => format!("`{shown}` is a directory, not a file"),
        _ => format!("could not {action} `{shown}`"),
    };

    Produced::failed(tool_name, ErrorCode::ToolFailed, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_sort_before_files() {
        let mut entries = [
            Entry {
                not_dir: true,
                name: "a.txt".to_owned(),
                size: Some(1),
                link: false,
            },
            Entry {
                not_dir: false,
                name: "z".to_owned(),
                size: None,
                link: false,
            },
        ];
        entries.sort();

        assert_eq!(entries[0].name, "z");
    }

    #[test]
    fn a_line_says_what_kind_of_entry_it_is() {
        let dir = Entry {
            not_dir: false,
            name: "src".to_owned(),
            size: None,
            link: false,
        };
        let file = Entry {
            not_dir: true,
            name: "a.txt".to_owned(),
            size: Some(12),
            link: false,
        };
        let link = Entry {
            not_dir: true,
            name: "l".to_owned(),
            size: Some(0),
            link: true,
        };

        assert_eq!(dir.render(), "src/");
        assert_eq!(file.render(), "a.txt\t12");
        assert_eq!(link.render(), "l@\t0");
    }

    #[test]
    fn a_window_cut_mid_character_is_still_text() {
        let text = "héllo";
        let cut = &text.as_bytes()[..2]; // "h" plus the first byte of "é"

        assert_eq!(text_window(cut, false), Some("h".to_owned()));
    }

    #[test]
    fn a_window_starting_mid_character_drops_the_fragment() {
        let text = "héllo";
        let from_inside = &text.as_bytes()[2..]; // the tail of "é", then "llo"

        assert_eq!(text_window(from_inside, true), Some("llo".to_owned()));
    }

    #[test]
    fn a_leading_fragment_is_only_trimmed_when_an_offset_asked_for_it() {
        let bytes = [0x80, b'a'];

        assert_eq!(text_window(&bytes, false), None, "not text from the start");
        assert_eq!(text_window(&bytes, true), Some("a".to_owned()));
    }

    #[test]
    fn a_binary_window_is_not_text() {
        // 0xFF is not a legal UTF-8 byte anywhere, and it is not at the end,
        // so it cannot be an incomplete character.
        assert_eq!(text_window(&[b'a', 0xFF, b'b'], false), None);
    }
}
