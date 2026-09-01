//! Token-endpoint calls. One POST per CLI, same public client ids they use.

use serde_json::{json, Value};

use super::json::string_field;

/// What a refresh endpoint handed back.
#[derive(Debug, Clone)]
pub struct Bundle {
    /// New access token.
    pub access_token: String,
    /// New refresh token, when the server rotated it.
    pub refresh_token: Option<String>,
    /// Lifetime in seconds, when the server said.
    pub expires_in: Option<u64>,
    /// Replacement id token, Codex only.
    pub id_token: Option<String>,
}

/// Claude Code's public OAuth client.
pub const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Codex CLI's public OAuth client.
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Grok CLI's public OAuth client (device / auth-code against `auth.x.ai`).
pub const GROK_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

const CLAUDE_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const GROK_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

/// Refreshes a Claude Code access token.
pub async fn claude(client: &reqwest::Client, refresh_token: &str) -> Result<Bundle, String> {
    json_refresh(
        client,
        CLAUDE_TOKEN_URL,
        json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLAUDE_CLIENT_ID,
        }),
    )
    .await
}

/// Refreshes a Codex / ChatGPT access token.
pub async fn codex(client: &reqwest::Client, refresh_token: &str) -> Result<Bundle, String> {
    json_refresh(
        client,
        CODEX_TOKEN_URL,
        json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CODEX_CLIENT_ID,
        }),
    )
    .await
}

/// Refreshes a Grok CLI access token.
///
/// Form-encoded: `auth.x.ai` is an OAuth2 server, not the JSON flavour Claude
/// and Codex speak.
pub async fn grok(
    client: &reqwest::Client,
    refresh_token: &str,
    client_id: &str,
    token_url: Option<&str>,
) -> Result<Bundle, String> {
    let url = token_url.unwrap_or(GROK_TOKEN_URL);
    let id = if client_id.is_empty() {
        GROK_CLIENT_ID
    } else {
        client_id
    };

    let response = client
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(format!(
            "grant_type=refresh_token&refresh_token={}&client_id={}",
            urlencoding(refresh_token),
            urlencoding(id),
        ))
        .send()
        .await
        .map_err(|err| format!("The token server could not be reached ({err})."))?;

    parse_token_response(response).await
}

async fn json_refresh(client: &reqwest::Client, url: &str, body: Value) -> Result<Bundle, String> {
    let response = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|err| format!("The token server could not be reached ({err})."))?;

    parse_token_response(response).await
}

async fn parse_token_response(response: reqwest::Response) -> Result<Bundle, String> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| format!("The token server's answer could not be read ({err})."))?;

    if !status.is_success() {
        return Err(format!(
            "The CLI login could not be refreshed ({status}). Run the official CLI once and try again."
        ));
    }

    let value: Value = serde_json::from_str(&text)
        .map_err(|_| "The token server answered, but not with JSON Aegis can read.".to_owned())?;

    let access_token = string_field(&value, "access_token")
        .ok_or_else(|| "The token server did not return an access token.".to_owned())?
        .to_owned();

    Ok(Bundle {
        access_token,
        refresh_token: string_field(&value, "refresh_token").map(str::to_owned),
        expires_in: value.get("expires_in").and_then(Value::as_u64).or_else(|| {
            value
                .get("expires_in")
                .and_then(Value::as_i64)
                .and_then(|n| u64::try_from(n).ok())
        }),
        id_token: string_field(&value, "id_token").map(str::to_owned),
    })
}

/// Percent-encodes a token for `application/x-www-form-urlencoded`.
///
/// Refresh tokens are URL-safe in practice; this still escapes anything that
/// would break a form body.
fn urlencoding(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// HTTP client used when the process-wide one is missing.
pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|err| format!("Aegis could not create an HTTP client ({err})."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_leaves_a_typical_token_alone() {
        assert_eq!(urlencoding("abc-_.~XYZ"), "abc-_.~XYZ");
    }

    #[test]
    fn form_encoding_escapes_a_plus() {
        assert_eq!(urlencoding("a+b"), "a%2Bb");
    }
}
