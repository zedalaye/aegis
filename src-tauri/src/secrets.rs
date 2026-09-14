//! The API key: where it is kept, and what may be said about it.
//!
//! * **The OS stores the key**; no Aegis document contains it.
//! * **The WebView never gets it**: only a [`KeySource`] and a [`key_hint`].
//! * **No credential store is supported** (headless Linux, rebuilt macOS dev
//!   binaries; PLAN 5.2, 5.3): [`ENV_API_KEY`] is a first-class source.
//!
//! Only [`ENV_API_KEY`] is read — never `OPENAI_API_KEY`, which would send a
//! key to an endpoint its owner did not choose.

use std::fmt;

use serde::Serialize;
use ts_rs::TS;

use crate::error::{AppError, AppResult};

/// Service name the key is filed under in the OS credential store.
///
/// The application's name, as shown in Credential Manager or Keychain Access.
pub const KEYRING_SERVICE: &str = "Aegis";

/// Account name within [`KEYRING_SERVICE`].
///
/// Named after the role; a later provider roster adds accounts (PLAN 7.1).
pub const KEYRING_ACCOUNT: &str = "provider-api-key";

/// The environment variable consulted when the credential store holds nothing.
pub const ENV_API_KEY: &str = "AEGIS_API_KEY";

/// Characters of the key left visible at the front by [`key_hint`].
const HINT_HEAD: usize = 3;

/// Characters left visible at the end.
const HINT_TAIL: usize = 4;

/// Below this length, showing both ends would show most of the key, so only
/// the tail is revealed.
const HINT_BOTH_ENDS_FROM: usize = 12;

/// Where the key in use came from (PLAN 2.1, `MaskedSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum KeySource {
    /// The OS credential store — Credential Manager, Keychain, Secret Service.
    Keyring,
    /// The [`ENV_API_KEY`] environment variable of this process.
    Env,
    /// Claude Code's own login on this machine.
    ClaudeCli,
    /// The Codex CLI's own login on this machine.
    CodexCli,
    /// The Grok CLI's own login on this machine.
    GrokCli,
    /// Neither. No request can be sent until one of them holds a key.
    None,
}

/// An API key, in memory.
///
/// Cannot be printed by accident: redacting `Debug`, no `Display` or
/// `Serialize`; it leaves only through [`ApiKey::expose`]. Not zeroed on drop,
/// since copies already exist upstream.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key, rejecting one that is only whitespace.
    ///
    /// Empty or blank means unset.
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            None
        } else {
            Some(Self(trimmed.to_owned()))
        }
    }

    /// The key itself, for the one place that sends it: the `Authorization`
    /// header in [`openai`](crate::agent::provider::openai).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// What may be shown of a key: a few characters, never enough to use.
///
/// `sk-…4f2a`, or `…4f2a` for short keys; the length is not revealed.
pub fn key_hint(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let tail: String = chars
        .iter()
        .skip(chars.len().saturating_sub(HINT_TAIL))
        .collect();

    if chars.len() >= HINT_BOTH_ENDS_FROM {
        let head: String = chars.iter().take(HINT_HEAD).collect();
        format!("{head}…{tail}")
    } else if chars.len() > HINT_TAIL {
        format!("…{tail}")
    } else {
        // A key this short is a placeholder or a mistake. Showing four
        // characters of it would be showing all of it.
        "…".to_owned()
    }
}

/// What the credential store and the environment hold right now.
///
/// One read answers everything, since each macOS keychain read may prompt.
#[derive(Debug)]
pub struct Held {
    /// The key, if either store had one.
    pub key: Option<ApiKey>,
    /// Which store answered.
    pub source: KeySource,
    /// Whether the platform's credential store answered at all.
    ///
    /// `false` on headless Linux or a locked keychain; an empty store is
    /// available.
    pub keyring_available: bool,
}

/// The credential store, plus the environment behind it.
///
/// Stateless: every call asks the platform again, since a keychain can unlock
/// while Aegis runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretStore;

impl SecretStore {
    /// A handle to this machine's credential store.
    pub const fn new() -> Self {
        Self
    }

    /// Reads the key, and everything that can be said about where it is.
    ///
    /// The credential store wins over the environment. Store failures are
    /// logged and fall through to [`ENV_API_KEY`].
    pub fn inspect(&self) -> Held {
        let (stored, keyring_available) =
            match Self::entry().as_ref().map(keyring::Entry::get_password) {
                Ok(Ok(password)) => (ApiKey::new(password), true),
                Ok(Err(keyring::Error::NoEntry)) => (None, true),
                Ok(Err(err)) => {
                    tracing::warn!(%err, "the credential store did not answer");
                    (None, false)
                }
                Err(err) => {
                    tracing::warn!(%err, "no credential store on this machine");
                    (None, false)
                }
            };

        if stored.is_some() {
            return Held {
                key: stored,
                source: KeySource::Keyring,
                keyring_available,
            };
        }

        match Self::env_key() {
            Some(key) => Held {
                key: Some(key),
                source: KeySource::Env,
                keyring_available,
            },
            None => Held {
                key: None,
                source: KeySource::None,
                keyring_available,
            },
        }
    }

    /// Files a key in the credential store, replacing whatever is there.
    ///
    /// Fails with `E_KEYRING_UNAVAILABLE`; never falls back to a file.
    pub fn store(&self, key: &ApiKey) -> AppResult<()> {
        let entry = Self::entry().map_err(Self::unavailable)?;

        entry
            .set_password(key.expose())
            .map_err(Self::unavailable)?;
        tracing::info!("the API key was saved to the credential store");
        Ok(())
    }

    /// Removes the stored key. Removing one that is not there succeeds.
    ///
    /// A key in [`ENV_API_KEY`] is untouched.
    pub fn clear(&self) -> AppResult<()> {
        let entry = Self::entry().map_err(Self::unavailable)?;

        match entry.delete_credential() {
            Ok(()) => {
                tracing::info!("the API key was removed from the credential store");
                Ok(())
            }
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(Self::unavailable(err)),
        }
    }

    /// The credential-store entry this application uses.
    fn entry() -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
    }

    /// The key in [`ENV_API_KEY`], if it is set to something.
    fn env_key() -> Option<ApiKey> {
        std::env::var(ENV_API_KEY).ok().and_then(ApiKey::new)
    }

    /// Turns any credential-store failure into the one code the UI branches
    /// on, keeping the platform's own words for the log rather than for the
    /// WebView — they name files and services, and a user cannot act on them.
    fn unavailable(err: keyring::Error) -> AppError {
        tracing::error!(%err, "the credential store refused the operation");
        AppError::Keyring
    }
}

#[cfg(test)]
mod tests {
    //! Nothing here touches the machine's real credential store: a test that
    //! wrote one would leave a key behind in the developer's Credential
    //! Manager or Keychain. What is covered is the pure half — masking, and
    //! what counts as a key at all.

    use super::*;

    #[test]
    fn a_long_key_shows_both_ends_and_nothing_between() {
        let hint = key_hint("sk-proj-abcdefghijklmnop4f2a");

        assert_eq!(hint, "sk-…4f2a");
        assert!(!hint.contains("abcdef"), "{hint}");
    }

    #[test]
    fn a_short_key_shows_only_its_tail() {
        assert_eq!(key_hint("abcdef7890"), "…7890");
    }

    /// A hint is for recognizing which key is installed. Anything short enough
    /// that four characters would be most of it gets no characters at all.
    #[test]
    fn a_tiny_key_shows_nothing() {
        for key in ["", "a", "abcd"] {
            assert_eq!(key_hint(key), "…", "{key:?} leaked");
        }
    }

    /// The hint is built from characters rather than bytes: slicing a
    /// multi-byte key at byte three would panic, and a panic in the settings
    /// panel is a settings panel that cannot be opened.
    #[test]
    fn a_key_of_multi_byte_characters_does_not_panic() {
        assert_eq!(key_hint("clé-très-longue-😀🔑"), "clé…e-😀🔑");
    }

    #[test]
    fn whitespace_is_not_a_key() {
        for raw in ["", " ", "\n", "\t "] {
            assert!(ApiKey::new(raw).is_none(), "{raw:?} was accepted as a key");
        }
    }

    /// A key pasted out of a terminal or a web page arrives with a newline on
    /// it more often than not, and a trailing newline in an `Authorization`
    /// header is a 401 that looks exactly like a wrong key.
    #[test]
    fn a_pasted_key_is_trimmed() {
        let key = ApiKey::new("  sk-abcd1234\n").expect("a key");
        assert_eq!(key.expose(), "sk-abcd1234");
    }

    /// The type exists to make an accidental print impossible. If this ever
    /// fails, a key is one `tracing::debug!` away from the log.
    #[test]
    fn a_key_cannot_be_printed_by_accident() {
        let key = ApiKey::new("sk-secret-value-1234").expect("a key");

        let debugged = format!("{key:?}");
        assert_eq!(debugged, "ApiKey(<redacted>)");
        assert!(!debugged.contains("secret"), "{debugged}");
    }

    #[test]
    fn the_environment_variable_carries_this_applications_name() {
        assert_eq!(ENV_API_KEY, "AEGIS_API_KEY");
        assert_ne!(
            ENV_API_KEY, "OPENAI_API_KEY",
            "a key exported for another tool was not chosen for this endpoint"
        );
    }

    #[test]
    fn key_sources_cross_the_wire_as_snake_case() {
        assert_eq!(
            serde_json::to_value(KeySource::Keyring).expect("serializes"),
            serde_json::json!("keyring")
        );
        assert_eq!(
            serde_json::to_value(KeySource::None).expect("serializes"),
            serde_json::json!("none")
        );
    }
}
