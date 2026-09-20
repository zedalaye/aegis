//! Codex CLI credentials: `~/.codex/auth.json` (or `$CODEX_HOME/auth.json`).

use std::path::{Path, PathBuf};

use base64::engine::{general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};

use crate::error::ErrorCode;
use crate::secrets::ApiKey;

use super::json::{read_value, string_field, write_value};
use super::refresh;
use super::{now_unix, Missing, Peek, Resolved};

/// Path Codex itself uses. `$CODEX_HOME` relocates the whole directory.
pub fn auth_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(dir).join("auth.json"));
    }
    super::home_dir().map(|home| home.join(".codex").join("auth.json"))
}

/// Reads the access token without a network call.
pub fn peek() -> Option<Peek> {
    let value = read_value(&auth_path()?)?;
    parse_peek(&value)
}

/// Reads, refreshes if needed, writes back.
pub async fn resolve(client: Option<&reqwest::Client>) -> Result<Resolved, Missing> {
    let path = auth_path().ok_or_else(missing)?;
    let mut value = read_value(&path).ok_or_else(missing)?;
    let peek = parse_peek(&value).ok_or_else(missing)?;
    let account_id = account_id_of(&value).ok_or_else(|| Missing {
        code: ErrorCode::NoApiKey.as_str(),
        message: "The Codex login has no ChatGPT account id. Run `codex login` again.".to_owned(),
    })?;

    if !peek.needs_refresh(now_unix()) {
        return Ok(Resolved::Codex {
            access_token: peek.access_token,
            account_id,
        });
    }

    let refresh_token = tokens_object(&value)
        .and_then(|tokens| string_field(tokens, "refresh_token"))
        .ok_or_else(|| Missing {
            code: ErrorCode::NoApiKey.as_str(),
            message: "The Codex login has expired, and there is no refresh token to renew it. \
                      Run `codex login` once and try again."
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

    let bundle = refresh::codex(http, refresh_token)
        .await
        .map_err(|message| {
            tracing::warn!(%message, "Codex token refresh failed");
            Missing {
                code: ErrorCode::NoApiKey.as_str(),
                message,
            }
        })?;

    patch_and_write(&mut value, &path, &bundle);
    let account_id = bundle
        .id_token
        .as_deref()
        .and_then(account_id_from_jwt)
        .unwrap_or(account_id);

    ApiKey::new(bundle.access_token)
        .ok_or_else(missing)
        .map(|access_token| Resolved::Codex {
            access_token,
            account_id,
        })
}

fn tokens_object(value: &Value) -> Option<&Value> {
    value.get("tokens")
}

fn parse_peek(value: &Value) -> Option<Peek> {
    let tokens = tokens_object(value)?;
    let access = string_field(tokens, "access_token")?;
    let expires_at = jwt_exp(access).or_else(|| {
        tokens
            .get("expires_at")
            .and_then(Value::as_i64)
            .or_else(|| value.get("expires_at").and_then(Value::as_i64))
    });

    Some(Peek {
        access_token: ApiKey::new(access)?,
        expires_at,
    })
}

fn account_id_of(value: &Value) -> Option<String> {
    let tokens = tokens_object(value)?;
    string_field(tokens, "account_id")
        .map(str::to_owned)
        .or_else(|| string_field(tokens, "access_token").and_then(account_id_from_jwt))
        .or_else(|| string_field(tokens, "id_token").and_then(account_id_from_jwt))
}

/// `exp` claim of a JWT, Unix seconds.
fn jwt_exp(token: &str) -> Option<i64> {
    jwt_payload(token)?.get("exp").and_then(Value::as_i64)
}

/// ChatGPT account id hidden in a Codex access or id token.
fn account_id_from_jwt(token: &str) -> Option<String> {
    let payload = jwt_payload(token)?;
    let auth = payload.get("https://api.openai.com/auth")?;
    string_field(auth, "chatgpt_account_id")
        .or_else(|| string_field(auth, "account_id"))
        .map(str::to_owned)
}

fn jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn patch_and_write(value: &mut Value, path: &Path, bundle: &refresh::Bundle) {
    let tokens = value
        .as_object_mut()
        .map(|obj| obj.entry("tokens".to_owned()).or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut);

    if let Some(tokens) = tokens {
        tokens.insert("access_token".to_owned(), json!(bundle.access_token));
        if let Some(refresh) = &bundle.refresh_token {
            tokens.insert("refresh_token".to_owned(), json!(refresh));
        }
        if let Some(id_token) = &bundle.id_token {
            tokens.insert("id_token".to_owned(), json!(id_token));
            if let Some(account) = account_id_from_jwt(id_token) {
                tokens.insert("account_id".to_owned(), json!(account));
            }
        }
    }

    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "last_refresh".to_owned(),
            json!(chrono::Utc::now().to_rfc3339()),
        );
    }

    if let Err(err) = write_value(path, value) {
        tracing::warn!(%err, path = %path.display(), "could not write the refreshed Codex login back");
    } else {
        tracing::info!("Codex credentials were refreshed");
    }
}

fn missing() -> Missing {
    Missing {
        code: ErrorCode::NoApiKey.as_str(),
        message: "No Codex login on this machine. Run `codex login` once, or switch Settings \
                  back to an API key."
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JWT whose payload is `{"exp":1700000000,"https://api.openai.com/auth":{"chatgpt_account_id":"acct_1"}}`.
    fn sample_jwt() -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"exp":1700000000,"https://api.openai.com/auth":{"chatgpt_account_id":"acct_1"}}"#,
        );
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn an_auth_json_yields_token_expiry_and_account() {
        let jwt = sample_jwt();
        let value = json!({
            "tokens": {
                "access_token": jwt,
                "refresh_token": "rt",
                "account_id": "acct_file"
            }
        });
        let peek = parse_peek(&value).expect("parsed");
        assert_eq!(peek.expires_at, Some(1_700_000_000));
        assert_eq!(account_id_of(&value).as_deref(), Some("acct_file"));
    }

    #[test]
    fn a_missing_account_id_is_read_from_the_jwt() {
        let jwt = sample_jwt();
        let value = json!({ "tokens": { "access_token": jwt } });
        assert_eq!(account_id_of(&value).as_deref(), Some("acct_1"));
    }
}
