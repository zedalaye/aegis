//! Claude Code credentials: `~/.claude/.credentials.json`, plus Keychain on macOS.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::ErrorCode;
use crate::secrets::ApiKey;

use super::json::{read_value, string_field, write_value};
use super::refresh;
use super::{now_unix, Missing, Peek, Resolved};

/// Linux/Windows (and macOS fallback) path. Overridable with `CLAUDE_CONFIG_DIR`.
pub fn credentials_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join(".credentials.json"));
    }
    super::home_dir().map(|home| home.join(".claude").join(".credentials.json"))
}

/// Reads the access token without a network call.
pub fn peek() -> Option<Peek> {
    let path = credentials_path()?;
    let value = read_file(&path).or_else(read_keychain)?;
    parse_peek(&value)
}

/// Reads, refreshes if needed, writes back.
pub async fn resolve(client: Option<&reqwest::Client>) -> Result<Resolved, Missing> {
    let path = credentials_path().ok_or_else(missing)?;
    let mut value = read_file(&path)
        .or_else(read_keychain)
        .ok_or_else(missing)?;
    let peek = parse_peek(&value).ok_or_else(missing)?;

    if !peek.needs_refresh(now_unix()) {
        return Ok(Resolved::Anthropic {
            access_token: peek.access_token,
        });
    }

    let refresh_token = oauth_object(&value)
        .and_then(|oauth| string_field(oauth, "refreshToken"))
        .ok_or_else(|| Missing {
            code: ErrorCode::NoApiKey.as_str(),
            message:
                "The Claude Code login has expired, and there is no refresh token to renew it. \
                      Run `claude` once and try again."
                    .to_owned(),
        })?;

    let owned;
    let http = match client {
        Some(client) => client,
        None => {
            owned = refresh::client()?;
            &owned
        }
    };

    let bundle = refresh::claude(http, refresh_token)
        .await
        .map_err(|message| {
            tracing::warn!(%message, "Claude Code token refresh failed");
            Missing {
                code: ErrorCode::NoApiKey.as_str(),
                message,
            }
        })?;

    patch_and_write(&mut value, &path, &bundle);

    ApiKey::new(bundle.access_token)
        .ok_or_else(missing)
        .map(|access_token| Resolved::Anthropic { access_token })
}

fn read_file(path: &Path) -> Option<Value> {
    let value = read_value(path)?;
    tracing::debug!(path = %path.display(), "read Claude Code credentials");
    Some(value)
}

/// macOS: Claude Code prefers the Keychain entry over the JSON file.
fn read_keychain() -> Option<Value> {
    #[cfg(target_os = "macos")]
    {
        let entry =
            keyring::Entry::new("Claude Code-credentials", "Claude Code-credentials").ok()?;
        let password = entry.get_password().ok()?;
        serde_json::from_str(&password).ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn oauth_object(value: &Value) -> Option<&Value> {
    value
        .get("claudeAiOauth")
        .or_else(|| value.get("claude_ai_oauth"))
}

fn parse_peek(value: &Value) -> Option<Peek> {
    let oauth = oauth_object(value)?;
    let access =
        string_field(oauth, "accessToken").or_else(|| string_field(oauth, "access_token"))?;
    let expires_at = oauth
        .get("expiresAt")
        .or_else(|| oauth.get("expires_at"))
        .and_then(ms_or_secs);

    Some(Peek {
        access_token: ApiKey::new(access)?,
        expires_at,
    })
}

/// Claude stores `expiresAt` in milliseconds. A value that looks like seconds
/// (ten digits) is accepted as seconds.
fn ms_or_secs(value: &Value) -> Option<i64> {
    let n = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))?;
    if n > 10_000_000_000 {
        Some(n / 1000)
    } else {
        Some(n)
    }
}

fn patch_and_write(value: &mut Value, path: &Path, bundle: &refresh::Bundle) {
    let expires_at_ms = bundle
        .expires_in
        .map(|secs| (now_unix().saturating_add(i64::try_from(secs).unwrap_or(0))) * 1000);

    let oauth = value
        .as_object_mut()
        .map(|obj| {
            obj.entry("claudeAiOauth".to_owned())
                .or_insert_with(|| json!({}))
        })
        .and_then(Value::as_object_mut);

    if let Some(oauth) = oauth {
        oauth.insert("accessToken".to_owned(), json!(bundle.access_token));
        if let Some(refresh) = &bundle.refresh_token {
            oauth.insert("refreshToken".to_owned(), json!(refresh));
        }
        if let Some(ms) = expires_at_ms {
            oauth.insert("expiresAt".to_owned(), json!(ms));
        }
    }

    if let Err(err) = write_value(path, value) {
        tracing::warn!(%err, path = %path.display(), "could not write the refreshed Claude Code login back");
    } else {
        tracing::info!("Claude Code credentials were refreshed");
    }
}

fn missing() -> Missing {
    Missing {
        code: ErrorCode::NoApiKey.as_str(),
        message: "No Claude Code login on this machine. Run `claude` once, or switch Settings \
                  back to an API key."
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credentials_file_yields_the_access_token_and_expiry() {
        let value = json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-abc",
                "refreshToken": "rt-1",
                "expiresAt": 1_700_000_000_000i64
            }
        });
        let peek = parse_peek(&value).expect("parsed");
        assert_eq!(peek.access_token.expose(), "sk-ant-oat01-abc");
        assert_eq!(peek.expires_at, Some(1_700_000_000));
    }

    #[test]
    fn snake_case_keys_are_accepted() {
        let value = json!({
            "claude_ai_oauth": {
                "access_token": "sk-ant-oat01-xyz",
                "expires_at": 1_700_000_000
            }
        });
        let peek = parse_peek(&value).expect("parsed");
        assert_eq!(peek.access_token.expose(), "sk-ant-oat01-xyz");
        assert_eq!(peek.expires_at, Some(1_700_000_000));
    }
}
