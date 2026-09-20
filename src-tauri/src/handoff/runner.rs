//! Running a brief: the [`Runner`](bus::Runner) the application supplies
//! (PLAN 7.3, Phase 15).
//!
//! Adapts the bus's policy to the existing machinery: a delegated run is an
//! ordinary session under the owner's own identity, [`Turn`] loop, matrix and
//! audit log — no second loop, no borrowed grants (`COS.md` *Roles*).
//!
//! [`Delegating`] does the work through [`Host`] (testable with plain stores);
//! [`AppRunner`] builds a `Host` from the [`AppHandle`].
//!
//! * The brief is filed in `.aegis/briefs/` when that exists
//!   ([`Delegating::file`]); otherwise it travels as the first message.
//! * A retry continues the same session.
//! * Only `handoff_return` (landing in [`handoff::Open`]) counts as an answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Manager as _, Runtime};
use tokio_util::sync::CancellationToken;

use crate::agent::event::EventSink;
use crate::agent::provider::Provider;
use crate::agent::turn::{self, Standing, Turn, TurnPlan};
use crate::agent::{StopReason, TurnRegistry};
use crate::approval::ApprovalRegistry;
use crate::audit::AuditLog;
use crate::commands::session::WindowSink;
use crate::exec_host::ExecHost;
use crate::handoff::{self, bus, Brief};
use crate::mcp::Connectors;
use crate::park::{Parking, Parks};
use crate::policy::GrantStore;
use crate::state::AppState;
use crate::store::{
    Agent, AgentStore, Delegated, MemoryStore, Message, SessionState, SessionStore,
};
use crate::workspace;
use crate::world;

/// Most characters of a goal that become a session title.
const TITLE_MAX_CHARS: usize = 48;

/// Most characters of a goal that become part of a filed brief's name.
const SLUG_MAX_CHARS: usize = 32;

/// Everything a delegated run borrows from the runtime around it.
///
/// References to process-lifetime stores; [`AppRunner`] only assembles it.
pub struct Host<'a> {
    /// Where an owner's name is resolved to an identity.
    pub agents: &'a AgentStore,
    /// Where the run's session is created and its transcript kept.
    pub sessions: &'a SessionStore,
    /// Which sessions are running; the specialist's turn registers here.
    pub turns: &'a TurnRegistry,
    /// Live session grants. A specialist's session holds none of the CoS's.
    pub grants: &'a GrantStore,
    /// Where the specialist's own approvals wait for an answer.
    pub approvals: &'a ApprovalRegistry,
    /// Where its tool calls are recorded, under the delegation's id.
    pub audit: &'a AuditLog,
    /// Where its events go, so the run is watchable while it happens.
    pub sink: &'a dyn EventSink,
    /// This application's binary, so `shell_exec` can refuse to run it.
    pub self_exe: Option<&'a Path>,
    /// Where `screen_capture` writes.
    pub captures: &'a Path,
    /// The skill library. A specialist runs the runbooks *it* was granted.
    pub skills: &'a Path,
    /// Where memories live; the run reads and writes the owner's own.
    pub memories: &'a MemoryStore,
    /// The running connectors (Phase 18); a specialist gets only its own
    /// identity's tools and grants.
    pub connectors: &'a Connectors,
    /// Which provider answers for an identity (per identity, PLAN 7.1).
    pub provider: &'a (dyn Fn(&Agent, &str) -> Box<dyn Provider> + Send + Sync),
    /// The decision client (PLAN 7.18), or `None` without a TypeSafe key.
    pub decision: Option<&'a crate::agent::decision::DecisionClient>,
    /// Where a dialog nobody answers is filed (PLAN 7.22). A brief is watched
    /// like any session, so only an expiry parks here.
    pub parked: &'a crate::store::ParkedStore,
    /// Who is told when a specialist's dialog goes unanswered.
    pub notifier: &'a dyn crate::notify::Notifier,
}

/// One session's delegations, and the runs they have opened.
///
/// Built per turn; remembers each brief's session so a retry continues it.
pub struct Delegating {
    /// The project the delegating session belongs to. Specialists work in it
    /// too — one workspace, one team (`COS.md` *Memory*: shared files).
    project_id: String,
    /// The session that is delegating. Recorded on every run it opens.
    from_session_id: String,
    /// That session's workspace root, or `None` when the folder is gone.
    workspace: Option<PathBuf>,
    /// Where the project's commands run (PLAN 7.12), same as the delegator's.
    exec_host: Option<ExecHost>,
    /// The session opened for each brief, keyed on its place in the fan-out.
    sessions: Mutex<HashMap<usize, String>>,
}

impl Delegating {
    /// A record of one session's delegations, with none yet made.
    pub fn new(
        project_id: String,
        from_session_id: String,
        workspace: Option<PathBuf>,
        exec_host: Option<ExecHost>,
    ) -> Self {
        Self {
            project_id,
            from_session_id,
            workspace,
            exec_host,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Writes the brief into `.aegis/briefs/`, when the workspace has one.
    ///
    /// Written by the runtime, not `fs_write`: the delegation was already
    /// approved and the dialog named this directory. Best effort.
    pub fn file(&self, slot: bus::Slot<'_>, brief: &Brief, rendered: &str) -> Option<String> {
        let root = self.workspace.as_ref()?;
        let dir = root.join(workspace::BRIEFS_DIR);
        if !dir.is_dir() {
            return None;
        }

        let name = format!(
            "{}-{}.md",
            &slot.handoff[..8.min(slot.handoff.len())],
            slug(&brief.goal)
        );
        let path = dir.join(&name);

        let body = format!(
            "# {}\n\nHanded to {} on {}.\n\n```\n{rendered}```\n",
            brief.goal.trim(),
            brief.owner.trim(),
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );

        match std::fs::write(&path, body) {
            Ok(()) => Some(format!("{}/{name}", workspace::BRIEFS_DIR)),
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "the brief could not be filed");
                None
            }
        }
    }

    /// One attempt at one brief: open the run, drive it, read the report.
    pub async fn attempt(
        &self,
        host: &Host<'_>,
        slot: bus::Slot<'_>,
        brief: &Brief,
        filed: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<handoff::Report, bus::Failure> {
        // The owner is resolved against the registry, by id or by name. An
        // owner nobody has heard of is fatal rather than retried: the second
        // attempt would look the same name up in the same document.
        let Some(agent) = owner(host.agents, &brief.owner) else {
            return Err(bus::Failure::fatal(format!(
                "there is no identity called `{}`. Name one from Settings → Identities, or do \
                 the work here",
                brief.owner.trim()
            )));
        };

        // Drifted world sources block the brief (PLAN 7.2), before any session
        // opens; not retryable, since a retry would hash the same files.
        if let Some(reason) = self
            .workspace
            .as_deref()
            .and_then(|root| world::blocking(root, &brief.inputs))
        {
            return Err(bus::Failure::fatal(reason));
        }

        let session_id = self.session_for(host, slot, brief, &agent.id, filed)?;
        let turn_id = uuid::Uuid::new_v4().to_string();

        // Registered like any other turn, so the sidebar shows the specialist
        // working and Stop reaches it. A busy session means a previous attempt
        // is somehow still running, which is a reason not to start a second.
        let own_cancel = match host.turns.begin(&session_id, &turn_id) {
            Ok(token) => token,
            Err(err) => return Err(bus::Failure::retryable(err.to_string())),
        };

        // The delegation's cancel reaches the turn's own: a Stop on the CoS, or
        // the bus's deadline, stops the specialist mid-call rather than at the
        // end of whatever it happens to be doing.
        let linked = own_cancel.clone();
        let watching = cancel.clone();
        let relay = tokio::spawn(async move {
            watching.cancelled().await;
            linked.cancel();
        });

        let message = opening(brief, filed, slot.attempt);
        let stored =
            host.sessions
                .append(&session_id, Message::user(message), SessionState::Running);
        if let Err(err) = stored {
            host.turns.finish(&session_id, &turn_id, SessionState::Idle);
            relay.abort();
            return Err(bus::Failure::fatal(format!(
                "its brief could not be recorded: {err}"
            )));
        }

        let open = handoff::Open::new(slot.handoff);
        let parks = Parks::new();
        let parking = Parking {
            store: host.parked,
            notifier: host.notifier,
            project_id: &self.project_id,
            routine_id: "",
            routine_name: "",
            skill: "",
            parks: &parks,
        };
        let provider = (host.provider)(&agent, &session_id);
        let plan = TurnPlan {
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            workspace: self.workspace.clone(),
            exec_host: self.exec_host.clone(),
        };

        let reason = Turn {
            agent: &agent,
            sessions: host.sessions,
            turns: host.turns,
            grants: host.grants,
            approvals: host.approvals,
            audit: host.audit,
            provider: provider.as_ref(),
            sink: host.sink,
            self_exe: host.self_exe,
            captures: host.captures,
            skills: host.skills,
            memories: host.memories,
            connectors: host.connectors,
            decision: host.decision,
            // What makes this a delegated run rather than a session: no bus, so
            // it cannot re-delegate, and a cell for the report it owes.
            standing: Standing::Delegated(&open),
            unattended: None,
            parking: Some(&parking),
        }
        .run(&plan, &own_cancel)
        .await;

        relay.abort();
        let resting = turn::resting_state(reason);
        host.turns.finish(&session_id, &turn_id, resting);
        if let Some(summary) = turn::summarize(host.sessions, &session_id, resting) {
            host.sink
                .emit(crate::agent::Event::SessionUpdated(Box::new(summary)));
        }

        open.take().ok_or_else(|| match reason {
            // The user pressed Stop, or the bus's deadline expired. Either way
            // nobody is waiting for a second attempt at it.
            StopReason::Cancelled => bus::Failure::fatal("it was stopped before it returned"),
            _ => bus::Failure::retryable(
                "it finished without calling `handoff_return`, so there is no report",
            ),
        })
    }

    /// The session this brief is being worked in, opening one if needed.
    fn session_for(
        &self,
        host: &Host<'_>,
        slot: bus::Slot<'_>,
        brief: &Brief,
        owner: &str,
        filed: Option<&str>,
    ) -> Result<String, bus::Failure> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(existing) = sessions.get(&slot.seq) {
            return Ok(existing.clone());
        }

        let summary = host
            .sessions
            .create_delegated(
                &self.project_id,
                Some(&title(&brief.goal)),
                owner,
                Delegated {
                    handoff_id: slot.handoff.to_owned(),
                    from_session_id: self.from_session_id.clone(),
                    brief: filed.map(str::to_owned),
                },
            )
            .map_err(|err| {
                // Nothing a second attempt would fix: the project is gone, or
                // the document cannot be written.
                bus::Failure::fatal(format!("its session could not be opened: {err}"))
            })?;

        // Emitted so a delegated run appears in the sidebar as it opens, rather
        // than after it has finished: work being done on your behalf should be
        // watchable while it happens.
        host.sink.emit(crate::agent::Event::SessionUpdated(Box::new(
            summary.clone(),
        )));

        sessions.insert(slot.seq, summary.id.clone());
        Ok(summary.id)
    }
}

/// [`Delegating`], wired to a running application.
///
/// State is looked up from the handle on each use, since a `State` borrow
/// cannot outlive the invocation.
pub struct AppRunner<R: Runtime> {
    app: AppHandle<R>,
    inner: Delegating,
}

impl<R: Runtime> AppRunner<R> {
    /// A runner for one session's delegations.
    pub fn new(
        app: AppHandle<R>,
        project_id: String,
        from_session_id: String,
        workspace: Option<PathBuf>,
        exec_host: Option<ExecHost>,
    ) -> Self {
        Self {
            app,
            inner: Delegating::new(project_id, from_session_id, workspace, exec_host),
        }
    }
}

impl<R: Runtime> bus::Runner for AppRunner<R> {
    fn file(&self, slot: bus::Slot<'_>, brief: &Brief, rendered: &str) -> Option<String> {
        self.inner.file(slot, brief, rendered)
    }

    fn run<'a>(
        &'a self,
        slot: bus::Slot<'a>,
        brief: &'a Brief,
        filed: Option<&'a str>,
        cancel: &'a CancellationToken,
    ) -> bus::Running<'a> {
        Box::pin(async move {
            let Some(state) = self.app.try_state::<AppState>() else {
                return Err(bus::Failure::fatal("the application is shutting down"));
            };

            let sink = WindowSink::new(self.app.clone());
            let notifier = crate::notify::Desktop::new(self.app.clone(), state.coalescer());
            let provider = |agent: &Agent, session_id: &str| state.provider_for(agent, session_id);
            let decision = state.decision_client();
            let host = Host {
                agents: state.agents(),
                parked: state.parked(),
                notifier: &notifier,
                sessions: state.sessions(),
                turns: state.turns(),
                grants: state.grants(),
                approvals: state.approvals(),
                audit: state.audit(),
                sink: &sink,
                self_exe: state.self_exe(),
                captures: state.captures(),
                skills: state.skills(),
                memories: state.memories(),
                connectors: state.connectors(),
                provider: &provider,
                decision: decision.as_ref(),
            };

            self.inner.attempt(&host, slot, brief, filed, cancel).await
        })
    }
}

/// The identity a brief names, by id or by name.
///
/// Names match case-insensitively (the registry forbids case-only duplicates).
fn owner(agents: &AgentStore, named: &str) -> Option<Agent> {
    let named = named.trim();
    agents
        .list()
        .into_iter()
        .find(|agent| agent.id == named || agent.name.eq_ignore_ascii_case(named))
}

/// What opens the owner's session, or nudges it on a second attempt.
///
/// The brief, plus that this is delegated work and must end with
/// `handoff_return`.
fn opening(brief: &Brief, filed: Option<&str>, attempt: u32) -> String {
    if attempt > 1 {
        return format!(
            "That attempt ended without a `handoff_return`, so nothing was reported back. This is \
             the last one. Finish now with `handoff_return`: `done` if the definition of done is \
             met, `blocked` if something you needed was missing, `needs_you` if it turns on a \
             decision only a person can make. A `blocked` with a clear question is a good answer; \
             another silence is not.\n\nThe brief again:\n\n{}",
            rendered(brief)
        );
    }

    let mut out = String::from(
        "You have been handed a brief. It is the whole of what you were asked for — there is no \
         conversation behind it to catch up on, and nothing in it grants you anything your \
         identity does not already hold.\n\n",
    );

    if let Some(path) = filed {
        out.push_str(&format!("It is also on file at `{path}`.\n\n"));
    }

    out.push_str(&rendered(brief));
    out.push_str(&format!(
        "\n{}\n\nWork through it, then finish with `handoff_return`. That call is the only thing \
         whoever briefed you will see: anything you write outside it is not reported. If a source \
         you need is missing, return `blocked` and say what and where — do not invent it.",
        brief.return_format.expectation()
    ));
    out
}

/// The brief as text, in `COS.md`'s shape.
///
/// Already checked by [`handoff::check_brief`]; the error arm falls back to the
/// goal rather than unwrapping.
fn rendered(brief: &Brief) -> String {
    handoff::check_brief(brief).unwrap_or_else(|reason| format!("goal: {}\n({reason})", brief.goal))
}

/// A goal, cut to a session title.
fn title(goal: &str) -> String {
    let goal = goal.trim();
    if goal.chars().count() <= TITLE_MAX_CHARS {
        return goal.to_owned();
    }
    format!(
        "{}…",
        goal.chars()
            .take(TITLE_MAX_CHARS - 1)
            .collect::<String>()
            .trim_end()
    )
}

/// A goal, cut to something that can be part of a file name.
fn slug(goal: &str) -> String {
    let mut out = String::with_capacity(SLUG_MAX_CHARS);
    let mut dash = false;

    for ch in goal.trim().chars() {
        if out.chars().count() >= SLUG_MAX_CHARS {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }

    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "brief".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff::{Priority, ReturnFormat};

    fn brief() -> Brief {
        Brief {
            goal: "Draft the release note for 0.4".to_owned(),
            owner: "Scribe".to_owned(),
            priority: Priority::Normal,
            inputs: vec![".aegis/artefacts/changelog.md".to_owned()],
            constraints: vec!["no marketing language".to_owned()],
            definition_of_done: ".aegis/artefacts/release-0.4.md exists".to_owned(),
            approval_needed: "the write".to_owned(),
            return_format: ReturnFormat::Artefact,
        }
    }

    #[test]
    fn a_slug_is_a_file_name_and_never_empty() {
        assert_eq!(
            slug("Draft the release note for 0.4"),
            "draft-the-release-note-for-0-4"
        );
        assert_eq!(slug("   "), "brief");
        assert_eq!(slug("///"), "brief");
        assert!(slug(&"x".repeat(200)).chars().count() <= SLUG_MAX_CHARS);
    }

    #[test]
    fn a_title_fits_a_sidebar() {
        assert_eq!(title("  short  "), "short");
        assert!(title(&"long ".repeat(40)).chars().count() <= TITLE_MAX_CHARS);
    }

    /// The opening message is the brief plus the two facts an owner cannot read
    /// off it: this is delegated, and only `handoff_return` reports back.
    #[test]
    fn the_opening_message_carries_the_brief_and_says_how_to_answer() {
        let message = opening(&brief(), Some(".aegis/briefs/ab12-draft.md"), 1);

        assert!(message.contains("goal: Draft the release note for 0.4"));
        assert!(message.contains(".aegis/artefacts/changelog.md"));
        assert!(message.contains(".aegis/briefs/ab12-draft.md"));
        assert!(message.contains("handoff_return"));
        assert!(message.contains("grants you anything"), "{message}");
    }

    #[test]
    fn a_second_attempt_says_it_is_the_last_one() {
        let message = opening(&brief(), None, 2);

        assert!(message.contains("last one"), "{message}");
        assert!(message.contains("blocked"), "{message}");
        assert!(message.contains("goal: Draft the release note"));
    }
}
