//! Tokens already on this machine, written by the official CLIs.
//!
//! Aegis does not run its own OAuth dance. Claude Code, Codex and the Grok CLI
//! have already done that; this module reads the files they keep, refreshes an
//! access token that has expired (same public `client_id` the CLI used), and
//! writes the new bundle back so the CLI and Aegis do not invalidate each
//! other.
//!
//! Nothing here is a substitute for an API key the user pasted into Settings.
//! [`AuthKind`](crate::store::AuthKind) is what picks this path, and the
//! token never crosses the WebView — only a [`KeySource`](crate::secrets::KeySource)
//! and a hint.

mod claude;
mod codex;
mod grok;
mod json;
mod refresh;

use crate::secrets::ApiKey;

pub use claude::{peek as peek_claude, resolve as resolve_claude};
pub use codex::{peek as peek_codex, resolve as resolve_codex};
pub use grok::{
    extra_headers as grok_headers, peek as peek_grok, resolve as resolve_grok,
    DEFAULT_BASE_URL as GROK_BASE_URL,
};

/// How long before expiry a token is treated as already gone.
///
/// Refreshing a minute early costs one HTTP round trip. Sending a token that
/// dies mid-stream costs the whole turn.
const REFRESH_SKEW_SECS: i64 = 60;

/// What a CLI login can give a provider.
#[derive(Debug, Clone)]
pub enum Resolved {
    /// Claude Code: `sk-ant-oat01-*`, consumed by motosan-ai in OAuth mode.
    Anthropic {
        /// The access token.
        access_token: ApiKey,
    },
    /// Codex / ChatGPT backend: bearer JWT plus the account id the Responses
    /// API wants in `chatgpt-account-id`.
    Codex {
        /// The access token.
        access_token: ApiKey,
        /// ChatGPT account id, from `auth.json` or the JWT.
        account_id: String,
    },
    /// Grok CLI: bearer for the CLI chat proxy (OpenAI-compatible).
    Grok {
        /// The access token.
        access_token: ApiKey,
        /// Where to POST `/chat/completions`. The official proxy unless the
        /// user overrode the base URL.
        base_url: String,
    },
    /// Gemini: an AI Studio key (`AIza…`), sent as `x-goog-api-key`.
    Gemini {
        /// The access token.
        access_token: ApiKey,
    },
}

/// A token sitting in a CLI file, before any refresh.
#[derive(Debug, Clone)]
pub struct Peek {
    /// The access token, for a hint and for a request that does not need a
    /// refresh.
    pub access_token: ApiKey,
    /// Unix seconds at which the access token is no longer usable, when known.
    pub expires_at: Option<i64>,
}

impl Peek {
    /// Whether this access token should be refreshed before it is sent.
    pub fn needs_refresh(&self, now: i64) -> bool {
        match self.expires_at {
            Some(exp) => exp - now <= REFRESH_SKEW_SECS,
            // No expiry recorded: send it. A 401 is the honest later answer,
            // and refreshing a token that is still good is how two processes
            // burn each other's refresh token.
            None => false,
        }
    }
}

/// Why a CLI login cannot be used, in words the settings panel can show.
#[derive(Debug, Clone)]
pub struct Missing {
    /// Stable code (`E_NO_API_KEY` — there is no credential, even if it is
    /// not an API key).
    pub code: &'static str,
    /// What to do: which CLI to run, which file was looked for.
    pub message: String,
}

impl Missing {
    /// A missing-or-unusable login, with the caller's words.
    pub fn no_key(message: impl Into<String>) -> Self {
        Self {
            code: crate::error::ErrorCode::NoApiKey.as_str(),
            message: message.into(),
        }
    }
}

impl From<String> for Missing {
    fn from(message: String) -> Self {
        Self::no_key(message)
    }
}

/// Reads the chosen CLI's store, refreshes if the access token is close to
/// expiry, and writes the new bundle back.
pub async fn resolve(
    kind: crate::store::AuthKind,
    client: Option<&reqwest::Client>,
    base_url_override: &str,
) -> Result<Resolved, Missing> {
    match kind {
        crate::store::AuthKind::ApiKey | crate::store::AuthKind::Gemini => Err(Missing {
            code: crate::error::ErrorCode::NoApiKey.as_str(),
            message: "Aegis is set to use an API key, not a CLI login.".to_owned(),
        }),
        crate::store::AuthKind::ClaudeCli => resolve_claude(client).await,
        crate::store::AuthKind::CodexCli => resolve_codex(client).await,
        crate::store::AuthKind::GrokCli => resolve_grok(client, base_url_override).await,
    }
}

/// Looks at the chosen CLI's store without touching the network.
pub fn peek(kind: crate::store::AuthKind) -> Option<Peek> {
    match kind {
        crate::store::AuthKind::ApiKey | crate::store::AuthKind::Gemini => None,
        crate::store::AuthKind::ClaudeCli => peek_claude(),
        crate::store::AuthKind::CodexCli => peek_codex(),
        crate::store::AuthKind::GrokCli => peek_grok().map(|found| found.peek),
    }
}

/// This user's home directory.
///
/// `USERPROFILE` on Windows, `HOME` everywhere else. `None` on a process that
/// has neither, which is exotic and reported as "no login found".
pub(crate) fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }
}

/// Unix seconds, for expiry math.
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_with_no_expiry_is_sent_as_is() {
        let peek = Peek {
            access_token: ApiKey::new("tok").expect("key"),
            expires_at: None,
        };
        assert!(!peek.needs_refresh(1_700_000_000));
    }

    #[test]
    fn a_token_inside_the_skew_window_is_refreshed() {
        let peek = Peek {
            access_token: ApiKey::new("tok").expect("key"),
            expires_at: Some(1_000),
        };
        assert!(peek.needs_refresh(1_000 - REFRESH_SKEW_SECS));
        assert!(peek.needs_refresh(1_000));
        assert!(!peek.needs_refresh(1_000 - REFRESH_SKEW_SECS - 1));
    }
}
