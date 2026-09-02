//! Per-session "always allow" grants (PLAN 3.1).
//!
//! A grant is what the user creates by answering an approval with
//! `allow_session`. Three properties make it something a user can reason
//! about, and all three are enforced here rather than in the UI:
//!
//! * **Narrow.** The key is `(tool, scope)`, never the tool alone. Approving
//!   `git` does not approve `rm`; approving writes in the workspace does not
//!   approve writes to `.git/` or anywhere outside it. Every grant carries a
//!   [`Grant::scope_label`] that says, in words, exactly what it covers — the
//!   same sentence the approval dialog showed before it was created.
//! * **Session-lifetime only.** Grants live in memory, keyed by session id,
//!   and are dropped when the session closes or the process exits. Nothing in
//!   this module touches the disk. There is no "always allow forever" in the
//!   MVP, and adding one would need a deliberate change here, not a new call
//!   site.
//! * **Revocable.** [`GrantStore::list`] is what Settings renders, and
//!   [`GrantStore::revoke`] is the button beside each row.
//!
//! A grant can only ever *skip a prompt policy would otherwise raise*. The
//! matrix decides first and names the grant that would cover the call; a row
//! that offers no grant — anything outside the workspace, any `.git/` write —
//! simply has no name to match, so no entry in this store can apply to it.
//! That is why the "never outside the workspace" rule needs no check of its
//! own: it is a property of the table, not a condition evaluated here.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One `allow_session` grant.
///
/// The variants are the scopes, not the tools: `fs_read` appears only as
/// [`Grant::FsReadLarge`] because the only `fs_read` row that offers a grant
/// is the large-file one, and a grant that covered every read would be a
/// different, much broader thing than what the user was asked about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Grant {
    /// Read any contained file over the size threshold without asking again.
    ///
    /// Reads under the threshold are auto-allowed anyway, and a sensitive name
    /// is asked about every time, so this grant covers exactly the row it was
    /// offered on: "this file is big".
    FsReadLarge,
    /// Write anywhere in the workspace subtree, except `.git/` and `world/`.
    FsWrite,
    /// Amend the workspace's constitution for the rest of the session
    /// (PLAN 7.2).
    ///
    /// Its own variant rather than a wider [`Grant::FsWrite`], and the split is
    /// the point of it existing. Somebody who allowed writes so a session could
    /// file artefacts has not thereby agreed to let it rewrite what the project
    /// *is*; somebody helping author an essence has not thereby opened every
    /// other file in the repository. Neither grant matches the other's row.
    ///
    /// It exists at all because founding a world is six files and amending one
    /// is rarely fewer — and six identical dialogs in a row is how a person
    /// learns to click through the one that mattered. What it never covers is a
    /// delegated run, which is refused before any grant is consulted, or an
    /// unattended one, which is offered nothing to sign: `COS.md` is that
    /// amending the world is a *human* decision, and a routine has no human in
    /// it.
    WorldAmend,
    /// Run one program in the workspace.
    Shell {
        /// The normalized program key — see [`Grant::shell`].
        program: String,
    },
    /// Capture the primary display.
    ScreenCapture,
    /// Record memories as this identity, for the rest of the session.
    ///
    /// Not scoped further, because there is nothing narrower to scope it to: a
    /// memory has no path and no program, only a sentence, and a grant keyed on
    /// the sentence would be a grant that never matched twice.
    MemoryWrite,
    /// Hand briefs to other identities for the rest of the session.
    ///
    /// Not scoped to an owner or a goal, for the reason [`Grant::MemoryWrite`]
    /// is not scoped to a sentence: what a user approves here is *this session
    /// may route work*, and a grant keyed on the goal would be one that never
    /// matched twice. It stays narrow anyway, because it covers only the
    /// routing — every tool call the specialists then make is judged by this
    /// same table, under their own identities and with none of this session's
    /// grants (they run in sessions of their own).
    HandoffDelegate,
    /// Call one tool of one connector for the rest of the session
    /// (PLAN 7.3, Phase 18).
    ///
    /// Keyed on the whole tool name — `git__status`, not `git` — and that is
    /// the decision of the phase rather than a detail of it. A connector's tool
    /// list is the server's to change, and it may change while a session is
    /// open (`notifications/tools/list_changed`). A grant that covered the
    /// *connector* would then quietly cover a tool that did not exist when
    /// somebody read the dialog. This one covers what was on the screen.
    ///
    /// Not scoped further than that, for the reason [`Grant::MemoryWrite`] is
    /// not scoped to a sentence: the arguments belong to a schema this process
    /// has never seen, and a grant keyed on them would be one that never
    /// matched twice.
    Connector {
        /// The full tool name the dialog named.
        tool: String,
    },
}

impl Grant {
    /// Builds a shell grant from the program as the model spelled it.
    ///
    /// The key is the basename, so `/usr/bin/git`, `git` and (on Windows)
    /// `C:\Program Files\Git\cmd\git.exe` all name the same grant — the user
    /// approved *running git*, and which copy of git PATH finds is not a
    /// distinction they were shown. Windows also drops the executable suffix
    /// and folds case, because `git.exe`, `git.cmd` and `GIT` are one program
    /// there. Phase 7 resolves the program through PATH before it runs; it
    /// normalizes through this same function, so the grant a user created on
    /// the prompt is the grant the resolved call matches.
    pub fn shell(program: &str) -> Self {
        Self::Shell {
            program: shell_key(program),
        }
    }

    /// The tool this grant can ever apply to.
    ///
    /// Borrowed rather than `&'static str` since Phase 18: a connector's tools
    /// are named by the server that offers them.
    pub fn tool(&self) -> &str {
        match self {
            Self::FsReadLarge => "fs_read",
            // One tool, two scopes that never overlap: the matrix names which
            // of them a given path's row offers, and a held grant only ever
            // matches the row it was created on.
            Self::FsWrite | Self::WorldAmend => "fs_write",
            Self::Shell { .. } => "shell_exec",
            Self::ScreenCapture => "screen_capture",
            Self::MemoryWrite => "memory_write",
            Self::HandoffDelegate => "handoff_delegate",
            Self::Connector { tool } => tool,
        }
    }

    /// What granting this would allow, in words.
    ///
    /// This is the `scope_label` on the approval request. It is written as a
    /// promise about the rest of the session, because that is what the user is
    /// actually agreeing to, and it is the same string Settings shows beside
    /// the Revoke button afterwards.
    pub fn scope_label(&self) -> String {
        match self {
            Self::FsReadLarge => {
                "read any file over 1 MB inside this workspace, for the rest of this session"
                    .to_owned()
            }
            Self::FsWrite => {
                "write any file inside this workspace, except under .git/ and world/, for the \
                 rest of this session"
                    .to_owned()
            }
            Self::WorldAmend => {
                "amend world/, this workspace's constitution, for the rest of this session — \
                 every other file is still asked about on its own"
                    .to_owned()
            }
            Self::Shell { program } => format!(
                "run `{program}` in this workspace, with any arguments, for the rest of this \
                 session"
            ),
            Self::ScreenCapture => {
                "capture the primary display, for the rest of this session".to_owned()
            }
            Self::MemoryWrite => {
                "remember things as this identity, for the rest of this session — you can read \
                 and correct them in Settings"
                    .to_owned()
            }
            Self::HandoffDelegate => {
                "hand briefs to other identities, for the rest of this session — what each of                  them then does is still approved call by call"
                    .to_owned()
            }
            Self::Connector { tool } => {
                let (connector, name) = crate::store::connectors::split_tool_name(tool)
                    .unwrap_or((tool.as_str(), tool.as_str()));
                format!(
                    "call `{name}` on the `{connector}` connector, with any arguments, for the \
                     rest of this session — no other tool of that connector, and nothing it adds \
                     later"
                )
            }
        }
    }
}

/// Normalizes a program name into a grant key.
///
/// Kept beside [`Grant::shell`] so the matrix, the approval path and Phase 7's
/// PATH resolution cannot drift into three slightly different answers.
fn shell_key(program: &str) -> String {
    let trimmed = program.trim();
    let basename = std::path::Path::new(trimmed)
        .file_name()
        .map_or(trimmed, |name| name.to_str().unwrap_or(trimmed));

    if cfg!(windows) {
        let stem = basename
            .rsplit_once('.')
            .map_or(basename, |(stem, _extension)| stem);
        stem.to_lowercase()
    } else {
        basename.to_owned()
    }
}

/// The live grants of every open session.
///
/// Held in [`AppState`](crate::AppState) for the life of the process. Each
/// session's set is independent: two sessions on the same workspace do not
/// share approvals, because the user approved a thing they were doing, not a
/// property of the folder.
#[derive(Debug, Default)]
pub struct GrantStore {
    sessions: Mutex<HashMap<String, HashSet<Grant>>>,
}

impl GrantStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the map.
    ///
    /// A poisoned mutex means some other command panicked while holding it.
    /// What is behind it is a plain map that is only ever inserted into or
    /// removed from wholesale, so it cannot be torn; recovering is strictly
    /// better than turning one panic into a permanently broken policy layer
    /// that denies every later call.
    fn sessions(&self) -> MutexGuard<'_, HashMap<String, HashSet<Grant>>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether `session` already granted `grant`.
    pub fn holds(&self, session: &str, grant: &Grant) -> bool {
        self.sessions()
            .get(session)
            .is_some_and(|grants| grants.contains(grant))
    }

    /// Records a grant. Returns `false` when it was already held.
    pub fn insert(&self, session: &str, grant: Grant) -> bool {
        let added = self
            .sessions()
            .entry(session.to_owned())
            .or_default()
            .insert(grant);
        if added {
            tracing::info!(session, "a session grant was created");
        }
        added
    }

    /// Withdraws a grant. Returns `false` when there was nothing to withdraw.
    pub fn revoke(&self, session: &str, grant: &Grant) -> bool {
        let mut sessions = self.sessions();
        let Some(grants) = sessions.get_mut(session) else {
            return false;
        };
        let removed = grants.remove(grant);
        if grants.is_empty() {
            sessions.remove(session);
        }
        if removed {
            tracing::info!(session, "a session grant was revoked");
        }
        removed
    }

    /// Every grant a session holds, in a stable order.
    ///
    /// Sorted because the set's own order is not reproducible, and this list
    /// is rendered: rows that reshuffle between refreshes are unusable.
    pub fn list(&self, session: &str) -> Vec<Grant> {
        let mut grants: Vec<Grant> = self
            .sessions()
            .get(session)
            .map(|grants| grants.iter().cloned().collect())
            .unwrap_or_default();
        grants.sort();
        grants
    }

    /// Drops everything a session held. Called when the session closes.
    pub fn clear(&self, session: &str) {
        if self.sessions().remove(session).is_some() {
            tracing::debug!(session, "session grants dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_is_held_only_by_the_session_that_made_it() {
        let store = GrantStore::new();
        assert!(store.insert("s1", Grant::FsWrite));

        assert!(store.holds("s1", &Grant::FsWrite));
        assert!(
            !store.holds("s2", &Grant::FsWrite),
            "grants must not leak between sessions"
        );
    }

    #[test]
    fn shell_grants_are_keyed_on_the_program_alone() {
        let store = GrantStore::new();
        store.insert("s1", Grant::shell("git"));

        assert!(store.holds("s1", &Grant::shell("/usr/bin/git")));
        assert!(
            !store.holds("s1", &Grant::shell("rm")),
            "approving one program must not approve another"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_keys_ignore_case_and_the_executable_suffix() {
        assert_eq!(Grant::shell("GIT.EXE"), Grant::shell("git"));
        assert_eq!(
            Grant::shell(r"C:\Program Files\Git\cmd\git.cmd"),
            Grant::shell("git")
        );
    }

    #[test]
    fn revoking_the_last_grant_forgets_the_session() {
        let store = GrantStore::new();
        store.insert("s1", Grant::ScreenCapture);

        assert!(store.revoke("s1", &Grant::ScreenCapture));
        assert!(
            !store.revoke("s1", &Grant::ScreenCapture),
            "revoke is idempotent"
        );
        assert!(store.list("s1").is_empty());
    }

    #[test]
    fn listing_is_stable() {
        let store = GrantStore::new();
        store.insert("s1", Grant::shell("pnpm"));
        store.insert("s1", Grant::FsWrite);
        store.insert("s1", Grant::shell("git"));

        let once = store.list("s1");
        assert_eq!(once, store.list("s1"));
        assert_eq!(once.len(), 3);
    }

    #[test]
    fn closing_a_session_drops_its_grants() {
        let store = GrantStore::new();
        store.insert("s1", Grant::FsWrite);
        store.insert("s2", Grant::FsWrite);

        store.clear("s1");

        assert!(!store.holds("s1", &Grant::FsWrite));
        assert!(
            store.holds("s2", &Grant::FsWrite),
            "only one session closed"
        );
    }
}
