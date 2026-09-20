//! Per-session "allow for this session" grants (PLAN 3.1).
//!
//! A grant is **narrow** (a scope, never a whole tool), **session-lifetime**
//! (in memory, dropped on close; nothing here touches disk) and **revocable**.
//! It can only collapse an ask whose row names it: a row offering no grant —
//! outside the workspace, a `.git/` write — has nothing to match.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One `allow_session` grant. The variants are scopes, not tools.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Grant {
    /// Contained reads over the size threshold.
    FsReadLarge,
    /// Write anywhere in the workspace subtree, except `.git/` and `world/`.
    FsWrite,
    /// Writes under `world/` (PLAN 7.2). Its own scope: a workspace-write grant
    /// never reaches the constitution, and this one reaches nothing else. Never
    /// offered to delegated or unattended runs.
    WorldAmend,
    /// Run one program in the workspace.
    Shell {
        /// The normalized program key — see [`Grant::shell`].
        program: String,
    },
    /// Capture the primary display.
    ScreenCapture,
    /// Record memories as this identity.
    MemoryWrite,
    /// Hand briefs to other identities. Covers the routing only: each
    /// specialist's calls are judged in its own session, without this grant.
    HandoffDelegate,
    /// One connector tool by full name (PLAN 7.3, Phase 18), never the whole
    /// connector: a server may add tools while a session is open.
    Connector {
        /// The full tool name the dialog named.
        tool: String,
    },
    /// One signed project eval by name (PLAN 7.18): its named input files go
    /// to TypeSafe.
    JevEval {
        /// The eval's name.
        name: String,
    },
    /// Model-written questions to TypeSafe (PLAN 7.18, the soupape).
    JevAsk,
}

impl Grant {
    /// A shell grant. A bare name is keyed on [`program_name`]; a name with a
    /// separator on its whole path, which the matrix passes resolved — so
    /// `scripts\git.cmd` is not `git`.
    pub fn shell(program: &str) -> Self {
        Self::Shell {
            program: shell_key(program),
        }
    }

    /// The tool this grant can apply to.
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
            Self::JevEval { .. } => "jev_eval",
            Self::JevAsk => "jev_ask",
        }
    }

    /// The clause every [`Grant::scope_label`] ends on, and the one thing that
    /// is not true of a grant signed onto a routine.
    const SESSION_CLAUSE: &'static str = "for the rest of this session";

    /// What the grant covers when it is signed onto a routine (PLAN 7.22)
    /// rather than held by one session: the same scope, on every run.
    ///
    /// The scope is written once, in [`Grant::scope_label`]; only the clause
    /// about how long it lasts differs, and
    /// [`every_scope_says_how_long_it_lasts`] keeps that substitution honest.
    ///
    /// [`every_scope_says_how_long_it_lasts`]: self::tests::every_scope_says_how_long_it_lasts
    pub fn standing_label(&self) -> String {
        self.scope_label()
            .replace(Self::SESSION_CLAUSE, "on every run of this routine")
    }

    /// What the grant covers, in words: the request's `scope_label`, and the
    /// text beside Revoke.
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
            Self::Shell { program } if program == "git" => {
                "run read-only `git` in this workspace (status, log, diff, show, …) for the rest \
                 of this session — any other verb, an option before the verb, and a line that \
                 writes a file or runs a program are still asked about"
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
                "hand briefs to other identities, for the rest of this session — what each of \
                 them then does is still approved call by call"
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
            Self::JevEval { name } => format!(
                "run the signed eval `{name}`, sending its input files to TypeSafe, for the rest \
                 of this session — no other eval"
            ),
            Self::JevAsk => {
                "send model-written questions and state to TypeSafe, for the rest of this \
                 session"
                    .to_owned()
            }
        }
    }
}

/// Normalizes a program into a grant key. See [`Grant::shell`].
fn shell_key(program: &str) -> String {
    let trimmed = program.trim();
    if !names_a_path(trimmed) {
        return program_name(trimmed);
    }
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Whether `program` names a file by path rather than a program on PATH — the
/// rule `shell_exec` resolves by.
pub fn names_a_path(program: &str) -> bool {
    std::path::Path::new(program.trim())
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty())
}

/// Which program a name runs: the basename, on Windows without its suffix and
/// lower-cased. Answers "is this git / is this Aegis"; never the grant key of a
/// program named by path.
pub fn program_name(program: &str) -> String {
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

/// The live grants of every open session, held in
/// [`AppState`](crate::AppState). Sessions never share grants, even on one
/// workspace.
///
/// Two maps, because they are two different promises. A [`Grant`] is a scope
/// that lasts the session; a **one-shot** is an answer to one parked ask
/// (PLAN 7.22), keyed on that call's fingerprint and gone the moment it is
/// used.
#[derive(Debug, Default)]
pub struct GrantStore {
    sessions: Mutex<HashMap<String, HashSet<Grant>>>,
    once: Mutex<HashMap<String, HashSet<String>>>,
}

impl GrantStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the map, recovering from poison: the map cannot be left torn, and
    /// one panic must not break every later decision.
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

    /// Every grant a session holds, sorted so the rendered list is stable.
    pub fn list(&self, session: &str) -> Vec<Grant> {
        let mut grants: Vec<Grant> = self
            .sessions()
            .get(session)
            .map(|grants| grants.iter().cloned().collect())
            .unwrap_or_default();
        grants.sort();
        grants
    }

    /// Drops everything a session held, one-shots included. Called when the
    /// session closes.
    pub fn clear(&self, session: &str) {
        if self.sessions().remove(session).is_some() {
            tracing::debug!(session, "session grants dropped");
        }
        self.once().remove(session);
    }

    /// Locks the one-shot map, recovering from poison.
    fn once(&self) -> MutexGuard<'_, HashMap<String, HashSet<String>>> {
        self.once
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records an *allow once* answer to a parked ask (PLAN 7.22).
    ///
    /// `fingerprint` is [`audit::fingerprint`](crate::audit::fingerprint): the
    /// tool and the digest of the arguments a person read. Nothing else
    /// matches it, so a model that regenerates different arguments asks again.
    pub fn allow_once(&self, session: &str, fingerprint: &str) -> bool {
        let added = self
            .once()
            .entry(session.to_owned())
            .or_default()
            .insert(fingerprint.to_owned());
        if added {
            tracing::info!(session, "a parked call was allowed once");
        }
        added
    }

    /// Spends a one-shot answer, if this exact call has one. It is consumed
    /// here, before the tool runs, so a repeated call asks again.
    pub fn take_once(&self, session: &str, fingerprint: &str) -> bool {
        let mut once = self.once();
        let Some(held) = once.get_mut(session) else {
            return false;
        };
        let spent = held.remove(fingerprint);
        if held.is_empty() {
            once.remove(session);
        }
        if spent {
            tracing::info!(session, "a one-shot answer was spent");
        }
        spent
    }

    /// How many one-shot answers a session is holding. Diagnostics and tests.
    pub fn once_held(&self, session: &str) -> usize {
        self.once().get(session).map_or(0, HashSet::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every scope says how long it lasts, which is what
    /// [`Grant::standing_label`] rewrites for a routine (PLAN 7.22). A variant
    /// that stopped saying it would silently sign a session-shaped promise
    /// onto a clock.
    #[test]
    fn every_scope_says_how_long_it_lasts() {
        let every = [
            Grant::FsReadLarge,
            Grant::FsWrite,
            Grant::WorldAmend,
            Grant::shell("git"),
            Grant::shell("cargo"),
            Grant::ScreenCapture,
            Grant::MemoryWrite,
            Grant::HandoffDelegate,
            Grant::Connector {
                tool: "git__status".to_owned(),
            },
            Grant::JevEval {
                name: "invoice".to_owned(),
            },
            Grant::JevAsk,
        ];

        for grant in every {
            let scope = grant.scope_label();
            assert!(
                scope.contains(Grant::SESSION_CLAUSE),
                "{scope}: every scope says how long it holds"
            );
            let standing = grant.standing_label();
            assert!(
                standing.contains("on every run of this routine") && standing != scope,
                "{standing}: a signed routine is not a session"
            );
        }
    }

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
    fn a_bare_name_is_keyed_on_the_program() {
        let store = GrantStore::new();
        store.insert("s1", Grant::shell("git"));

        assert!(store.holds("s1", &Grant::shell(" git ")));
        assert!(
            !store.holds("s1", &Grant::shell("rm")),
            "approving one program must not approve another"
        );
    }

    #[test]
    fn a_program_named_by_a_path_is_keyed_on_the_path() {
        let store = GrantStore::new();
        store.insert("s1", Grant::shell("git"));

        assert!(
            !store.holds("s1", &Grant::shell("/ws/scripts/git")),
            "a file called git is not git"
        );
        assert_ne!(Grant::shell("/ws/a/tool"), Grant::shell("/ws/b/tool"));
        assert_eq!(program_name("/ws/scripts/git"), "git");
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_keys_ignore_case_and_the_executable_suffix() {
        assert_eq!(Grant::shell("GIT.EXE"), Grant::shell("git"));
        assert_eq!(
            Grant::shell(r"C:\WS\Tool.cmd"),
            Grant::shell(r"c:\ws\tool.cmd")
        );
        assert_ne!(
            Grant::shell(r"C:\Program Files\Git\cmd\git.cmd"),
            Grant::shell("git")
        );
        assert_eq!(program_name(r"C:\Program Files\Git\cmd\git.cmd"), "git");
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

    /// PLAN 7.22: an answer to a parked ask covers that one call.
    #[test]
    fn a_one_shot_answer_is_spent_the_first_time_it_matches() {
        let store = GrantStore::new();
        store.allow_once("s1", "fs_write:abc");

        assert!(!store.take_once("s1", "fs_write:def"), "another call");
        assert!(!store.take_once("s2", "fs_write:abc"), "another session");
        assert!(store.take_once("s1", "fs_write:abc"));
        assert!(
            !store.take_once("s1", "fs_write:abc"),
            "the same call a second time asks again"
        );
        assert_eq!(store.once_held("s1"), 0);
    }

    #[test]
    fn closing_a_session_drops_its_grants() {
        let store = GrantStore::new();
        store.insert("s1", Grant::FsWrite);
        store.insert("s2", Grant::FsWrite);

        store.allow_once("s1", "fs_write:abc");

        store.clear("s1");

        assert!(!store.holds("s1", &Grant::FsWrite));
        assert!(!store.take_once("s1", "fs_write:abc"));
        assert!(
            store.holds("s2", &Grant::FsWrite),
            "only one session closed"
        );
    }
}
