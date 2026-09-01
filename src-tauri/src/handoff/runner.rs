//! Running a brief: the [`Runner`](bus::Runner) the application supplies
//! (PLAN 7.3, Phase 15).
//!
//! This is the adapter between the bus's policy — parallel, bounded, two
//! attempts, then the human — and the machinery that actually answers a brief.
//! The machinery is the one that was already there. A delegated run is an
//! ordinary session, bound to the owner's identity, driven by the ordinary
//! [`Turn`] loop, judged by the ordinary policy matrix, written to the ordinary
//! audit log.
//!
//! That is the whole design of the phase, and it is worth being explicit about
//! what it rules out. There is no second agent loop, no worker pool and no
//! privileged path for delegated work. A specialist writing a file raises the
//! same dialog, under its own identity's allow-list, that the same call would
//! raise in a session you typed into — `COS.md` *Roles*: the CoS may see state,
//! and it may not act through somebody else's grants. Nothing here can widen an
//! identity, because nothing here constructs one: it looks the owner up in the
//! registry and runs as whatever is on file.
//!
//! ## Two types, and why
//!
//! [`Delegating`] is the work: file a brief, open a session, drive a turn, read
//! the report out of the cell. It borrows what it needs through [`Host`], a
//! struct of references, for the reason [`Turn`] takes one — the caller
//! assembles it once, and it can be assembled from a running application or
//! from a directory of stores in a test. [`AppRunner`] is the second, and it is
//! four lines of adapter: look the state up on the [`AppHandle`] and build a
//! `Host` from it.
//!
//! ## Three things worth reading the code for
//!
//! **The brief is a file first.** [`Delegating::file`] writes it into `briefs/`
//! when the workspace has one, and the run then starts from a path rather than
//! from a paragraph (`COS.md`: inputs are paths, never paste). A workspace with
//! no convention still works — the brief travels as the run's first message —
//! because scaffolding is the user's choice, not the harness's (PLAN 7.3,
//! Phase 11).
//!
//! **A retry is the same session continued.** [`bus`] allows two attempts; the
//! second reuses the session the first opened, so whatever the first attempt
//! did get written is still there. What the owner is told is that it ran out of
//! time and that a `blocked` is a better answer than another silence.
//!
//! **A run that never returns is not a run that succeeded quietly.** The only
//! way out with a report is `handoff_return`, which lands in [`handoff::Open`].
//! A turn that ends without one is a failed attempt, and two of those are a
//! line on the board asking the human.

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
use crate::handoff::{self, bus, Brief};
use crate::mcp::Connectors;
use crate::policy::GrantStore;
use crate::state::AppState;
use crate::store::{
    Agent, AgentStore, Delegated, MemoryStore, Message, SessionState, SessionStore,
};
use crate::workspace;

/// Most characters of a goal that become a session title.
const TITLE_MAX_CHARS: usize = 48;

/// Most characters of a goal that become part of a filed brief's name.
const SLUG_MAX_CHARS: usize = 32;

/// Everything a delegated run borrows from the runtime around it.
///
/// The same shape, and the same reasoning, as [`Turn`]: the fields are almost
/// all references to stores that live for the process, and a function taking
/// eleven of them positionally is a call site nobody can read. Assembling it is
/// also the only thing [`AppRunner`] does, which is what lets everything below
/// be exercised without an application.
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
    /// The connectors this installation is running (PLAN 7.3, Phase 18).
    ///
    /// The same roster the delegating session sees. A specialist is offered the
    /// connector tools *its own* identity holds, and every call it makes stops
    /// and asks in its own session — none of the CoS's grants travel with the
    /// brief, and a connector's tool is no exception.
    pub connectors: &'a Connectors,
    /// Which provider answers for an identity.
    ///
    /// A function rather than a provider, because the binding is per identity
    /// (PLAN 7.1, *Provider*): the CoS and the specialist it briefs may be on
    /// different models, and resolving one provider for the delegation would
    /// quietly decide otherwise.
    pub provider: &'a (dyn Fn(&Agent) -> Box<dyn Provider> + Send + Sync),
}

/// One session's delegations, and the runs they have opened.
///
/// Built per turn: it remembers the session opened for each brief, so the
/// second attempt the bus allows continues that run rather than starting a
/// third one beside it.
pub struct Delegating {
    /// The project the delegating session belongs to. Specialists work in it
    /// too — one workspace, one team (`COS.md` *Memory*: shared files).
    project_id: String,
    /// The session that is delegating. Recorded on every run it opens.
    from_session_id: String,
    /// That session's workspace root, or `None` when the folder is gone.
    workspace: Option<PathBuf>,
    /// The session opened for each brief, keyed on its place in the fan-out.
    sessions: Mutex<HashMap<usize, String>>,
}

impl Delegating {
    /// A record of one session's delegations, with none yet made.
    pub fn new(project_id: String, from_session_id: String, workspace: Option<PathBuf>) -> Self {
        Self {
            project_id,
            from_session_id,
            workspace,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Writes the brief into `briefs/`, when the workspace has one.
    ///
    /// Directly rather than through `fs_write`, and that is worth being clear
    /// about: this is the *runtime* recording a delegation the user has already
    /// approved, into the one directory the convention reserves for exactly
    /// this, under a name it chooses. It is not the model reaching the disk —
    /// nothing it writes can go anywhere but a brief file — and the approval
    /// dialog named the directory it would land in.
    ///
    /// Best effort. A brief that could not be filed costs the file and nothing
    /// else: the run still starts, from the same text, in its first message.
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
        let provider = (host.provider)(&agent);
        let plan = TurnPlan {
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            workspace: self.workspace.clone(),
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
            // What makes this a delegated run rather than a session: no bus, so
            // it cannot re-delegate, and a cell for the report it owes.
            standing: Standing::Delegated(&open),
            unattended: None,
        }
        .run(&plan, &own_cancel)
        .await;

        relay.abort();
        let resting = turn::resting_state(reason);
        host.turns.finish(&session_id, &turn_id, resting);
        if let Some(summary) = turn::summarize(host.sessions, &session_id, resting) {
            host.sink.emit(crate::agent::Event::SessionUpdated(summary));
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
        host.sink
            .emit(crate::agent::Event::SessionUpdated(summary.clone()));

        sessions.insert(slot.seq, summary.id.clone());
        Ok(summary.id)
    }
}

/// [`Delegating`], wired to a running application.
///
/// The state is looked up from the handle on each use rather than captured, for
/// the reason the turn task does it: a `State<'_, AppState>` borrows an
/// invocation that this outlives.
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
    ) -> Self {
        Self {
            app,
            inner: Delegating::new(project_id, from_session_id, workspace),
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
            let provider = |agent: &Agent| state.provider_for(agent);
            let host = Host {
                agents: state.agents(),
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
            };

            self.inner.attempt(&host, slot, brief, filed, cancel).await
        })
    }
}

/// The identity a brief names, by id or by name.
///
/// Case-insensitively by name, because a CoS writes `Reviewer` the way a person
/// would, and the registry already refuses two identities whose names differ
/// only in case.
fn owner(agents: &AgentStore, named: &str) -> Option<Agent> {
    let named = named.trim();
    agents
        .list()
        .into_iter()
        .find(|agent| agent.id == named || agent.name.eq_ignore_ascii_case(named))
}

/// What opens the owner's session, or nudges it on a second attempt.
///
/// The brief itself, plus the two facts the owner cannot read off it: that this
/// is delegated work, and that the only way to finish is `handoff_return`. A
/// specialist that ends a turn with prose has said nothing anybody is listening
/// for — the CoS reads reports, not transcripts.
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
/// It has already passed [`handoff::check_brief`] by the time a run starts, so
/// the error arm is unreachable; it is written rather than unwrapped because an
/// owner staring at an empty message would be a worse failure than one staring
/// at a goal.
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
            inputs: vec!["artefacts/changelog.md".to_owned()],
            constraints: vec!["no marketing language".to_owned()],
            definition_of_done: "artefacts/release-0.4.md exists".to_owned(),
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
        let message = opening(&brief(), Some("briefs/ab12-draft.md"), 1);

        assert!(message.contains("goal: Draft the release note for 0.4"));
        assert!(message.contains("artefacts/changelog.md"));
        assert!(message.contains("briefs/ab12-draft.md"));
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
