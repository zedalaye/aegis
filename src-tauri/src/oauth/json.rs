//! Shared JSON file helpers for CLI credential stores.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde_json::Value;

/// Reads a JSON document, tolerating a UTF-8 BOM.
pub fn read_value(path: &Path) -> Option<Value> {
    let bytes = fs::read(path).ok()?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).ok()
}

/// Writes `value` over `path` so readers see either the old file or the new
/// one, and restores owner-only permissions on Unix (the CLIs write `0600`).
pub fn write_value(path: &Path, value: &Value) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("credential path has no parent"))?;
    fs::create_dir_all(dir)?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }

    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        err
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// A JSON string field, rejecting empty values.
pub fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}
