//! Grok CLI credentials: `~/.grok/auth.json` (or `$GROK_HOME/auth.json`).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::error::ErrorCode;
use crate::secrets::ApiKey;

use super::json::{read_value, string_field, write_value};
use super::refresh;
use super::{now_unix, Missing, Peek, Resolved};

/// Official Grok CLI chat proxy. OpenAI-compatible `/chat/completions`.
pub const DEFAULT_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";

/// Floor the proxy currently advertises on a 426.
///
/// Sent only when `~/.grok/version.json` cannot be read. A request with no
/// `x-grok-client-version` is reported as version `(none)` and refused.
const MINIMUM_CLIENT_VERSION: &str = "0.1.202";

/// How the official CLI names itself on the proxy.
const CLIENT_IDENTIFIER: &str = "grok-shell";

/// Headers the proxy requires in addition to `Authorization: Bearer`.
///
/// `X-XAI-Token-Auth` is the session-token flavour. The version header is
/// what turns a 426 ("CLI version (none) is outdated") into a real request:
/// the proxy treats a missing one as older than [`MINIMUM_CLIENT_VERSION`].
pub fn extra_headers() -> Vec<(String, String)> {
    let version = client_version();
    vec![
        ("X-XAI-Token-Auth".to_owned(), "xai-grok-cli".to_owned()),
        ("x-grok-client-version".to_owned(), version.clone()),
        (
            "x-grok-client-identifier".to_owned(),
            CLIENT_IDENTIFIER.to_owned(),
        ),
        ("User-Agent".to_owned(), format!("xai-grok-cli/{version}")),
    ]
}

/// Version the proxy will see for this machine.
///
/// Prefers the Grok CLI's own `version.json`, so a `grok update` is picked up
/// on the next request without Aegis spawning the binary.
fn client_version() -> String {
    grok_home()
        .and_then(|home| read_value(&home.join("version.json")))
        .and_then(|value| parse_version(&value))
        .unwrap_or_else(|| MINIMUM_CLIENT_VERSION.to_owned())
}

fn parse_version(value: &Value) -> Option<String> {
    string_field(value, "version").map(str::to_owned)
}

/// Directory the Grok CLI keeps `auth.json` and `version.json` in.
fn grok_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GROK_HOME") {
        return Some(PathBuf::from(dir));
    }
    super::home_dir().map(|home| home.join(".grok"))
}

/// A Grok CLI entry plus the map key it was stored under, so a refresh can
/// write back to the same slot.
#[derive(Debug, Clone)]
pub struct Found {
    /// The access token and its expiry.
    pub peek: Peek,
    /// Map key in `auth.json` (`issuer::client_id` or the legacy sign-in URL).
    pub slot: String,
    /// Refresh token, when the CLI stored one.
    pub refresh_token: Option<String>,
    /// OIDC client id, falling back to the Grok CLI's public one.
    pub client_id: String,
}

/// Path the Grok CLI itself uses.
pub fn auth_path() -> Option<PathBuf> {
    grok_home().map(|home| home.join("auth.json"))
}

/// Reads the newest usable entry without a network call.
pub fn peek() -> Option<Found> {
    let value = read_value(&auth_path()?)?;
    parse_found(&value)
}

/// Reads, refreshes if needed, writes back.
pub async fn resolve(
    client: Option<&reqwest::Client>,
    base_url_override: &str,
) -> Result<Resolved, Missing> {
    let path = auth_path().ok_or_else(missing)?;
    let mut value = read_value(&path).ok_or_else(missing)?;
    let found = parse_found(&value).ok_or_else(missing)?;
    let base_url = if base_url_override.trim().is_empty() {
        DEFAULT_BASE_URL.to_owned()
    } else {
        base_url_override.trim().trim_end_matches('/').to_owned()
    };

    if !found.peek.needs_refresh(now_unix()) {
        return Ok(Resolved::Grok {
            access_token: found.peek.access_token,
            base_url,
        });
    }

    let refresh_token = found.refresh_token.clone().ok_or_else(|| Missing {
        code: ErrorCode::NoApiKey.as_str(),
        message: "The Grok CLI login has expired, and there is no refresh token to renew it. \
                  Run `grok login` once and try again."
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

    let bundle = refresh::grok(http, &refresh_token, &found.client_id, None)
        .await
        .map_err(|message| {
            tracing::warn!(%message, "Grok token refresh failed");
            Missing {
                code: ErrorCode::NoApiKey.as_str(),
                message,
            }
        })?;

    patch_and_write(&mut value, &path, &found.slot, &bundle);

    ApiKey::new(bundle.access_token)
        .ok_or_else(missing)
        .map(|access_token| Resolved::Grok {
            access_token,
            base_url,
        })
}

fn parse_found(root: &Value) -> Option<Found> {
    let map = root.as_object()?;
    let mut best: Option<(DateTime<Utc>, String, &Value)> = None;

    for (slot, entry) in map {
        if slot == "xai::api_key" {
            continue;
        }
        if !entry.is_object() {
            continue;
        }
        if string_field(entry, "key").is_none() {
            continue;
        }
        let created = string_field(entry, "create_time")
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(DateTime::<Utc>::MIN_UTC);

        match &best {
            Some((best_time, _, _)) if created < *best_time => {}
            _ => best = Some((created, slot.clone(), entry)),
        }
    }

    let (_, slot, entry) = best?;
    let access = string_field(entry, "key")?;
    let expires_at = string_field(entry, "expires_at")
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp());

    Some(Found {
        peek: Peek {
            access_token: ApiKey::new(access)?,
            expires_at,
        },
        slot,
        refresh_token: string_field(entry, "refresh_token").map(str::to_owned),
        client_id: string_field(entry, "oidc_client_id")
            .unwrap_or(refresh::GROK_CLIENT_ID)
            .to_owned(),
    })
}

fn patch_and_write(root: &mut Value, path: &Path, slot: &str, bundle: &refresh::Bundle) {
    let Some(entry) = root.get_mut(slot).and_then(Value::as_object_mut) else {
        return;
    };

    entry.insert("key".to_owned(), json!(bundle.access_token));
    if let Some(refresh) = &bundle.refresh_token {
        entry.insert("refresh_token".to_owned(), json!(refresh));
    }
    if let Some(secs) = bundle.expires_in {
        let exp = Utc::now() + chrono::Duration::seconds(i64::try_from(secs).unwrap_or(0));
        entry.insert("expires_at".to_owned(), json!(exp.to_rfc3339()));
    }

    if let Err(err) = write_value(path, root) {
        tracing::warn!(%err, path = %path.display(), "could not write the refreshed Grok login back");
    } else {
        tracing::info!("Grok CLI credentials were refreshed");
    }
}

fn missing() -> Missing {
    Missing {
        code: ErrorCode::NoApiKey.as_str(),
        message: "No Grok CLI login on this machine. Run `grok login` once, or switch Settings \
                  back to an API key."
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_oidc_entry_wins_over_the_legacy_slot() {
        let value = json!({
            "https://accounts.x.ai/sign-in": {
                "key": "legacy-token",
                "create_time": "2026-01-01T00:00:00Z"
            },
            "https://auth.x.ai::abc": {
                "key": "oidc-token",
                "refresh_token": "rt",
                "create_time": "2026-06-01T00:00:00Z",
                "expires_at": "2026-06-08T00:00:00Z",
                "oidc_client_id": "client-1"
            },
            "xai::api_key": {
                "key": "xai-not-oauth",
                "create_time": "2026-08-01T00:00:00Z"
            }
        });
        let found = parse_found(&value).expect("parsed");
        assert_eq!(found.peek.access_token.expose(), "oidc-token");
        assert_eq!(found.client_id, "client-1");
        assert_eq!(found.refresh_token.as_deref(), Some("rt"));
        assert_eq!(found.slot, "https://auth.x.ai::abc");
    }

    #[test]
    fn extra_headers_name_a_cli_version() {
        let headers = extra_headers();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("X-XAI-Token-Auth"), Some("xai-grok-cli"));
        assert_eq!(get("x-grok-client-identifier"), Some(CLIENT_IDENTIFIER));
        let version = get("x-grok-client-version").expect("version");
        assert!(
            !version.is_empty() && version != "none",
            "the proxy treats a missing version as (none) and 426s: {version}"
        );
        let user_agent = format!("xai-grok-cli/{version}");
        assert_eq!(get("User-Agent"), Some(user_agent.as_str()));
    }

    #[test]
    fn version_json_is_the_cli_own_label() {
        let value = json!({
            "version": "1.0.13",
            "stable_version": "1.0.13"
        });
        assert_eq!(parse_version(&value).as_deref(), Some("1.0.13"));
        assert!(parse_version(&json!({"version": "  "})).is_none());
        assert!(parse_version(&json!({})).is_none());
    }
}
