//! The API key: where it is kept, and what may be said about it.
//!
//! Three rules shape this module, all of them from `AGENTS.md`, and it exists
//! to hold them in one place rather than to wrap `keyring`:
//!
//! * **The operating system stores the key, not Aegis.** No document this
//!   process writes ever contains it — `settings.json` holds a base URL and a
//!   model name and nothing else.
//! * **The key never reaches the WebView.** What crosses the IPC boundary is a
//!   [`KeySource`] and the four characters [`key_hint`] leaves visible. There
//!   is deliberately no command that reads a key back out.
//! * **A machine with no usable credential store is a supported machine.**
//!   Headless Linux has no Secret Service at all, and a macOS dev binary loses
//!   its keychain ACL on every rebuild (PLAN 5.2, 5.3). The environment
//!   variable is therefore a first-class source, not an emergency hatch, and
//!   [`KeySource`] reports honestly which one answered.
//!
//! Only [`ENV_API_KEY`] is read from the environment. Picking up a generic
//! `OPENAI_API_KEY` would be convenient and wrong: that key was exported for
//! whatever tool the user set it up for, and Aegis sends its key to whichever
//! base URL is configured here — which may be a different company's server.
//! Using someone's credential against an endpoint they did not choose is not a
//! convenience, so the variable carries this application's name.

use std::fmt;

use serde::Serialize;
use ts_rs::TS;

use crate::error::{AppError, AppResult};

/// Service name the key is filed under in the OS credential store.
///
/// Windows shows it in Credential Manager and macOS in Keychain Access, so it
/// is the application's name rather than a slug: a user auditing what is
/// stored on their machine should recognize the entry without decoding it.
pub const KEYRING_SERVICE: &str = "Aegis";

/// Account name within [`KEYRING_SERVICE`].
///
/// Named after the role rather than after a provider, because the roster of
/// providers is a post-MVP seam (PLAN 7.1): a second key later becomes a
/// second account under the same service, not a second service.
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
    /// Neither. No request can be sent until one of them holds a key.
    None,
}

/// An API key, in memory.
///
/// The newtype buys one thing, and it is worth a type: the key cannot be
/// printed by accident. [`fmt::Debug`] is written by hand and reveals nothing,
/// there is no `Display` and no `Serialize`, so a key can only leave through
/// [`ApiKey::expose`] — which is greppable, and which every caller has to
/// name.
///
/// The bytes are not zeroed on drop. Doing that honestly would require the key
/// never to have been copied on its way here, which is not true of a value
/// that came out of `keyring` or out of the process environment; a `Drop` impl
/// clearing one of several copies would buy the appearance of the guarantee
/// rather than the guarantee.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key, rejecting one that is only whitespace.
    ///
    /// An empty environment variable is how a shell says "unset" at least as
    /// often as it means an empty value, and an all-whitespace key is a paste
    /// accident. Both are `None` here rather than a key that produces a 401
    /// somewhere further along.
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
/// `sk-…4f2a` when the key is long enough that both ends can be shown without
/// showing the middle, `…4f2a` when it is shorter. The purpose is recognition
/// — telling *which* of your keys is installed — rather than verification, so
/// the length is not revealed either.
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
/// One value rather than three calls, because on macOS every read of the
/// keychain is a potential consent prompt: asking once and answering all three
/// questions from that one answer is the difference between a settings panel
/// that opens and one that interrogates the user.
#[derive(Debug)]
pub struct Held {
    /// The key, if either store had one.
    pub key: Option<ApiKey>,
    /// Which store answered.
    pub source: KeySource,
    /// Whether the platform's credential store answered at all.
    ///
    /// `false` on headless Linux and on a locked keychain. A store that is
    /// merely empty is available; the UI uses this to decide whether to offer
    /// to save a key or to explain [`ENV_API_KEY`] instead.
    pub keyring_available: bool,
}

/// The credential store, plus the environment behind it.
///
/// Holds no state and caches nothing. Every call asks the platform again,
/// which is the only honest answer to "is there a key right now": a keychain
/// can be unlocked and a Secret Service can be started while Aegis is running,
/// and a cached "no" would outlive both. The calls happen when the settings
/// panel is opened or a turn begins — rare enough that a platform round trip
/// costs less than remembering the wrong answer.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretStore;

impl SecretStore {
    /// A handle to this machine's credential store.
    pub const fn new() -> Self {
        Self
    }

    /// Reads the key, and everything that can be said about where it is.
    ///
    /// The credential store is consulted first: a key the user typed into
    /// Aegis wins over one their shell happens to export, because the first
    /// was chosen for this application and the second was not.
    ///
    /// Every credential-store failure means the same thing here — "not from
    /// here" — and is logged rather than propagated, because the environment
    /// is still to be tried and a locked keychain must not stop a working
    /// `AEGIS_API_KEY` from being found.
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
    /// Fails with `E_KEYRING_UNAVAILABLE` rather than falling back to a file:
    /// writing a key somewhere Aegis controls would break the first rule of
    /// this module, and a user told plainly that their machine has no
    /// credential store can set [`ENV_API_KEY`] instead.
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
    /// Idempotent because the user's intent — "there should be no key here" —
    /// is satisfied either way, and because a panel whose button fails on the
    /// second press is a panel that looks broken.
    ///
    /// A key in [`ENV_API_KEY`] is untouched and keeps working: Aegis does not
    /// edit the environment it was started in. The panel says so rather than
    /// leaving the user to wonder why the key came back.
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
