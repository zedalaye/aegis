//! The approval registry: where a turn waits for a human (PLAN 4.2, step 3).
//!
//! [`policy`](crate::policy) decides *whether* to ask and writes the wording.
//! This module owns everything that follows from asking: the id the dialog
//! answers, the deadline, the channel the turn is parked on, and the list a
//! reopened window re-syncs from. Keeping the two apart is what lets the
//! decision table stay a pure function of a call and a workspace.
//!
//! Three properties are worth stating plainly, because they are what the shape
//! of this module is for.
//!
//! **A pending approval is a rendezvous, not a queue.** Each request owns one
//! [`oneshot`] sender, held here until somebody answers. The turn holds the
//! receiver and is blocked on it. That is why an approval cannot be answered
//! twice: the answer consumes the sender, and the second attempt finds nothing
//! to answer.
//!
//! **Nothing here can time out on its own.** There is no reaper task. The turn
//! that is waiting applies the deadline, because the turn is the only party
//! that can act on it — it has the call to refuse, the transcript to write the
//! refusal into and the audit line to append. The registry only records
//! [`Pending::expires_at`] so that a late answer is refused as stale rather
//! than allowing a call whose prompt is no longer on screen.
//!
//! **`allow_session` is checked here, not in the WebView.** A request that
//! offers no grant carries `session_grant_allowed: false` so the button is not
//! drawn, and answering it `allow_session` anyway is [`AppError::GrantNotAllowed`]
//! (PLAN 3.1). The UI is where the rule is *shown*; this is where it holds.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use chrono::{SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::time::{Duration, Instant};
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::policy::{ApprovalDetail, AskRequest, Grant, Risk};

/// How long a request stays answerable (PLAN 4.2).
///
/// A prompt nobody answered in five minutes is a prompt nobody is looking at —
/// the user walked away, or the window is behind something. Expiry resolves as
/// a denial, which is the safe direction: the call does not run, the model is
/// told why, and the turn carries on.
pub const APPROVAL_TTL: Duration = Duration::from_secs(5 * 60);

/// What the user answered (PLAN 2.1, `Decision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Decision {
    /// Run this call, and ask again next time.
    AllowOnce,
    /// Run it, and stop asking for whatever the request's grant covers.
    AllowSession,
    /// Do not run it.
    Deny,
}

impl Decision {
    /// Whether this answer lets the call run.
    pub const fn allows(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowSession)
    }

    /// The audit vocabulary for this answer.
    pub const fn audit(self) -> crate::audit::AuditDecision {
        match self {
            Self::AllowOnce => crate::audit::AuditDecision::AllowOnce,
            Self::AllowSession => crate::audit::AuditDecision::AllowSession,
            Self::Deny => crate::audit::AuditDecision::Deny,
        }
    }
}

/// Who or what produced an answer (PLAN 2.2, `tool:approval_resolved`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ResolvedBy {
    /// Somebody clicked a button.
    User,
    /// The runtime answered on the user's behalf — a cancelled turn, a session
    /// that went away.
    Policy,
    /// The request expired unanswered.
    Timeout,
}

/// One approval, as the dialog receives it (PLAN 2.1, `ApprovalRequest`).
///
/// Half of this comes from [`AskRequest`] — the wording, the badge, the
/// structured detail, the scope — and half is added here: the id to answer
/// with, and the window during which answering it means anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ApprovalRequest {
    /// What `approval_resolve` is called with.
    pub request_id: String,
    /// The session whose turn is blocked.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// The model's own id for the call.
    pub call_id: String,
    /// The tool being asked about.
    pub tool: String,
    /// The badge. Advisory only (PLAN 3.3).
    pub risk: Risk,
    /// The dialog's title: "Write file", "Run shell command".
    pub title: String,
    /// One line naming the thing.
    pub summary: String,
    /// The structured detail the dialog draws.
    pub detail: ApprovalDetail,
    /// What "allow for this session" would cover, in words.
    pub scope_label: String,
    /// Whether `allow_session` is offered at all (PLAN 3.1).
    pub session_grant_allowed: bool,
    /// Why policy is asking.
    pub reason: String,
    /// When it was raised, fixed-width UTC RFC3339.
    pub requested_at: String,
    /// When it stops being answerable.
    pub expires_at: String,
}

/// An answer, on its way back to the turn that is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    /// What was decided.
    pub decision: Decision,
    /// Who decided it.
    pub resolved_by: ResolvedBy,
}

impl Answer {
    /// An answer from a person.
    pub const fn user(decision: Decision) -> Self {
        Self {
            decision,
            resolved_by: ResolvedBy::User,
        }
    }
}

/// A registered request, plus the two things that are not shown.
#[derive(Debug)]
struct Pending {
    /// What the dialog sees.
    request: ApprovalRequest,
    /// The grant an `allow_session` answer creates. `None` is exactly
    /// `session_grant_allowed: false` — there is no second field to disagree
    /// with it.
    grant: Option<Grant>,
    /// Where the answer goes.
    respond: oneshot::Sender<Answer>,
    /// The deadline, as a monotonic instant. The RFC3339 copy on the request
    /// is for the UI; this is the one that decides.
    expires_at: Instant,
}

/// A registration, held by the turn that is waiting.
///
/// Carries the request so the caller can emit `tool:approval_required` without
/// looking it up again, and the receiver it then awaits.
#[derive(Debug)]
pub struct Ticket {
    /// The request as it was registered.
    pub request: ApprovalRequest,
    /// Where the answer arrives.
    pub answer: oneshot::Receiver<Answer>,
}

/// The approvals every open session is blocked on.
///
/// One per process, held in [`AppState`](crate::AppState) beside the grant
/// store. Not persisted, for the same reason a grant is not: a request whose
/// turn died with the process cannot be answered, and offering to answer it
/// after a restart would be offering to approve a call nothing would run.
#[derive(Debug, Default)]
pub struct ApprovalRegistry {
    pending: Mutex<HashMap<String, Pending>>,
}

impl ApprovalRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the map.
    ///
    /// Recovering from a poisoned mutex for the same reason the other stores
    /// do: what is behind it is a plain map, and turning one panic into a
    /// runtime that can never approve anything again would be worse than the
    /// panic.
    fn pending(&self) -> MutexGuard<'_, HashMap<String, Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers what policy asked, and hands back the ticket to wait on.
    ///
    /// The ids come from the call being judged rather than from the request,
    /// because a request is only ever raised about a call that is already
    /// identified — inventing a second identity here would give the audit log
    /// and the transcript two different names for one event.
    pub fn register(
        &self,
        session_id: &str,
        turn_id: &str,
        call_id: &str,
        ask: &AskRequest,
    ) -> Ticket {
        let now = Utc::now();
        // `TimeDelta::from_std` only fails past ~292 billion years; the
        // fallback keeps the expiry monotonic rather than pretending the
        // request was born expired.
        let ttl = TimeDelta::from_std(APPROVAL_TTL).unwrap_or_else(|_| TimeDelta::minutes(5));

        let request = ApprovalRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            call_id: call_id.to_owned(),
            tool: ask.tool.to_owned(),
            risk: ask.risk,
            title: ask.title.to_owned(),
            summary: ask.summary.clone(),
            detail: ask.detail.clone(),
            scope_label: ask.scope_label.clone(),
            session_grant_allowed: ask.grant.is_some(),
            reason: ask.reason.clone(),
            requested_at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
            expires_at: (now + ttl).to_rfc3339_opts(SecondsFormat::Millis, true),
        };

        let (respond, answer) = oneshot::channel();
        self.pending().insert(
            request.request_id.clone(),
            Pending {
                request: request.clone(),
                grant: ask.grant.clone(),
                respond,
                expires_at: Instant::now() + APPROVAL_TTL,
            },
        );

        tracing::info!(
            session_id,
            turn_id,
            tool = ask.tool,
            request_id = %request.request_id,
            "waiting for an approval"
        );
        Ticket { request, answer }
    }

    /// Answers a request on a user's behalf.
    ///
    /// Returns the grant the answer created, if it created one, so the caller
    /// can report it without a second lookup. Three failures, all of them
    /// [`AppError`] rather than a silent success:
    ///
    /// * an id nothing is waiting on — expired, already answered, or from a
    ///   turn that has since been cancelled — is `E_APPROVAL_STALE`, and the
    ///   UI's response is to re-sync rather than to branch on it;
    /// * `allow_session` on a row that offers no grant is `E_GRANT_NOT_ALLOWED`,
    ///   and the request stays open, because the user has not actually answered
    ///   the question they were asked;
    /// * a turn that stopped listening between the lookup and the send is
    ///   stale too: nothing will run, whatever was clicked.
    pub fn resolve(
        &self,
        request_id: &str,
        decision: Decision,
        grants: &crate::policy::GrantStore,
    ) -> AppResult<Resolution> {
        let mut pending = self.pending();

        let entry = pending
            .get(request_id)
            .ok_or_else(|| AppError::ApprovalStale {
                request_id: request_id.to_owned(),
            })?;

        if decision == Decision::AllowSession && entry.grant.is_none() {
            tracing::warn!(
                request_id,
                tool = %entry.request.tool,
                "an allow-session answer arrived for a row that offers no grant"
            );
            return Err(AppError::GrantNotAllowed {
                tool: entry.request.tool.clone(),
            });
        }

        // Past this point the request is answered however it goes: an expired
        // one is removed rather than left for a second attempt to find.
        let entry = pending
            .remove(request_id)
            .ok_or_else(|| AppError::ApprovalStale {
                request_id: request_id.to_owned(),
            })?;
        drop(pending);

        if Instant::now() >= entry.expires_at {
            tracing::info!(request_id, "an approval was answered after it expired");
            return Err(AppError::ApprovalStale {
                request_id: request_id.to_owned(),
            });
        }

        // The grant is recorded before the turn is released, so the call that
        // was just approved and every call behind it see the same store.
        let granted = match (decision, entry.grant) {
            (Decision::AllowSession, Some(grant)) => {
                grants.insert(&entry.request.session_id, grant.clone());
                Some(grant)
            }
            _ => None,
        };

        if entry.respond.send(Answer::user(decision)).is_err() {
            tracing::info!(request_id, "the turn stopped waiting for this approval");
            return Err(AppError::ApprovalStale {
                request_id: request_id.to_owned(),
            });
        }

        tracing::info!(request_id, ?decision, "an approval was answered");
        Ok(Resolution {
            request: entry.request,
            decision,
            granted,
        })
    }

    /// Everything still waiting, oldest first, optionally for one session.
    ///
    /// Ordered because it is rendered and because the order is meaningful: a
    /// turn asks one call at a time, but a second session can be blocked too,
    /// and the queue a user works through should be the queue in the order it
    /// arrived.
    pub fn list(&self, session_id: Option<&str>) -> Vec<ApprovalRequest> {
        let mut open: Vec<ApprovalRequest> = self
            .pending()
            .values()
            .filter(|entry| session_id.is_none_or(|wanted| entry.request.session_id == wanted))
            .map(|entry| entry.request.clone())
            .collect();

        open.sort_by(|a, b| {
            a.requested_at
                .cmp(&b.requested_at)
                .then_with(|| a.request_id.cmp(&b.request_id))
        });
        open
    }

    /// Removes one request without answering it.
    ///
    /// What a turn calls when it has stopped waiting on its own account — the
    /// deadline passed, or the turn was cancelled. Dropping the sender is what
    /// makes any answer that arrives afterwards stale.
    pub fn withdraw(&self, request_id: &str) {
        if self.pending().remove(request_id).is_some() {
            tracing::debug!(request_id, "an approval was withdrawn");
        }
    }

    /// Removes every request a session is blocked on.
    ///
    /// Called when a session is deleted. The turn is cancelled first and would
    /// withdraw its own request; this is what covers the gap between the two,
    /// so a dialog cannot be answered for a session that no longer exists.
    pub fn withdraw_session(&self, session_id: &str) {
        let mut pending = self.pending();
        let before = pending.len();
        pending.retain(|_, entry| entry.request.session_id != session_id);

        let dropped = before - pending.len();
        if dropped > 0 {
            tracing::debug!(
                session_id,
                dropped,
                "a closed session's approvals were dropped"
            );
        }
    }

    /// How many requests are open. Diagnostics only.
    pub fn len(&self) -> usize {
        self.pending().len()
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What answering an approval produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The request that was answered.
    pub request: ApprovalRequest,
    /// The answer.
    pub decision: Decision,
    /// The grant it created, when it created one.
    pub granted: Option<Grant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::policy::{ApprovalDetail, GrantStore, Risk};

    /// The `AskRequest` a `fs_write` inside the workspace produces.
    fn write_ask() -> AskRequest {
        AskRequest {
            tool: "fs_write",
            risk: Risk::Medium,
            title: "Write file",
            summary: "notes.md (12 B, new file)".to_owned(),
            detail: ApprovalDetail::FsWrite {
                path: "/ws/notes.md".to_owned(),
                bytes: 12,
                exists: false,
                preview: Some("hello".to_owned()),
            },
            grant: Some(Grant::FsWrite),
            scope_label: Grant::FsWrite.scope_label(),
            reason: "this creates a file in the workspace".to_owned(),
        }
    }

    /// A row that offers no session grant — anything outside the workspace.
    fn ungrantable_ask() -> AskRequest {
        AskRequest {
            grant: None,
            scope_label: "this one call, and nothing else".to_owned(),
            reason: "this file is outside the workspace".to_owned(),
            ..write_ask()
        }
    }

    #[tokio::test]
    async fn an_answer_reaches_the_turn_that_is_waiting() {
        let registry = ApprovalRegistry::new();
        let grants = GrantStore::new();
        let ticket = registry.register("s1", "t1", "c1", &write_ask());

        let resolution = registry
            .resolve(&ticket.request.request_id, Decision::AllowOnce, &grants)
            .expect("the request is open");

        assert_eq!(resolution.decision, Decision::AllowOnce);
        assert_eq!(resolution.granted, None, "allow-once grants nothing");
        assert_eq!(
            ticket.answer.await.expect("the sender was held"),
            Answer::user(Decision::AllowOnce)
        );
        assert!(registry.is_empty(), "an answered request is not still open");
    }

    #[tokio::test]
    async fn allow_session_records_the_grant_the_request_named() {
        let registry = ApprovalRegistry::new();
        let grants = GrantStore::new();
        let ticket = registry.register("s1", "t1", "c1", &write_ask());

        let resolution = registry
            .resolve(&ticket.request.request_id, Decision::AllowSession, &grants)
            .expect("the request is open");

        assert_eq!(resolution.granted, Some(Grant::FsWrite));
        assert!(grants.holds("s1", &Grant::FsWrite));
        assert!(
            !grants.holds("s2", &Grant::FsWrite),
            "a grant belongs to the session that made it"
        );
    }

    /// PLAN 3.1: the Rust side rejects the decision rather than trusting the
    /// WebView to have hidden the button.
    #[tokio::test]
    async fn allow_session_is_refused_where_no_grant_is_on_offer() {
        let registry = ApprovalRegistry::new();
        let grants = GrantStore::new();
        let ticket = registry.register("s1", "t1", "c1", &ungrantable_ask());
        assert!(!ticket.request.session_grant_allowed);

        let err = registry
            .resolve(&ticket.request.request_id, Decision::AllowSession, &grants)
            .expect_err("this row offers no grant");
        assert_eq!(err.code(), crate::ErrorCode::GrantNotAllowed);

        // The question has not been answered, so it is still being asked.
        assert_eq!(registry.len(), 1);
        registry
            .resolve(&ticket.request.request_id, Decision::AllowOnce, &grants)
            .expect("the user can still answer it another way");
    }

    #[tokio::test]
    async fn an_unknown_request_is_stale_rather_than_silent() {
        let registry = ApprovalRegistry::new();
        let grants = GrantStore::new();

        let err = registry
            .resolve("nothing-is-waiting-on-this", Decision::AllowOnce, &grants)
            .expect_err("there is no such request");
        assert_eq!(err.code(), crate::ErrorCode::ApprovalStale);
    }

    #[tokio::test]
    async fn an_approval_can_only_be_answered_once() {
        let registry = ApprovalRegistry::new();
        let grants = GrantStore::new();
        let ticket = registry.register("s1", "t1", "c1", &write_ask());
        let id = ticket.request.request_id.clone();

        registry
            .resolve(&id, Decision::AllowOnce, &grants)
            .expect("the first answer wins");
        let err = registry
            .resolve(&id, Decision::Deny, &grants)
            .expect_err("the second finds nothing to answer");
        assert_eq!(err.code(), crate::ErrorCode::ApprovalStale);
    }

    /// A turn that stopped waiting — cancelled, or past its deadline — leaves
    /// no sender behind. Answering then must not report success: nothing would
    /// run.
    #[tokio::test]
    async fn answering_a_turn_that_stopped_listening_is_stale() {
        let registry = ApprovalRegistry::new();
        let grants = GrantStore::new();
        let ticket = registry.register("s1", "t1", "c1", &write_ask());
        let id = ticket.request.request_id.clone();
        drop(ticket.answer);

        let err = registry
            .resolve(&id, Decision::AllowOnce, &grants)
            .expect_err("nobody is listening");
        assert_eq!(err.code(), crate::ErrorCode::ApprovalStale);
    }

    #[tokio::test]
    async fn a_withdrawn_request_can_no_longer_be_answered() {
        let registry = ApprovalRegistry::new();
        let grants = GrantStore::new();
        let ticket = registry.register("s1", "t1", "c1", &write_ask());

        registry.withdraw(&ticket.request.request_id);

        let err = registry
            .resolve(&ticket.request.request_id, Decision::AllowOnce, &grants)
            .expect_err("it was withdrawn");
        assert_eq!(err.code(), crate::ErrorCode::ApprovalStale);
    }

    #[tokio::test]
    async fn listing_is_per_session_and_ordered() {
        let registry = ApprovalRegistry::new();
        registry.register("s1", "t1", "c1", &write_ask());
        registry.register("s2", "t2", "c2", &write_ask());
        registry.register("s1", "t1", "c3", &write_ask());

        assert_eq!(registry.list(None).len(), 3);

        let mine = registry.list(Some("s1"));
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().all(|request| request.session_id == "s1"));
        assert!(
            mine[0].requested_at <= mine[1].requested_at,
            "the queue is in the order it arrived"
        );
        assert_eq!(registry.list(Some("s3")), Vec::new());
    }

    #[tokio::test]
    async fn closing_a_session_drops_only_its_own_approvals() {
        let registry = ApprovalRegistry::new();
        registry.register("s1", "t1", "c1", &write_ask());
        registry.register("s2", "t2", "c2", &write_ask());

        registry.withdraw_session("s1");

        assert_eq!(registry.list(Some("s1")), Vec::new());
        assert_eq!(registry.list(Some("s2")).len(), 1);
    }

    /// The dialog is answered by `request_id`, and the transcript, the audit
    /// line and the `tool:finished` event are all keyed on `call_id`. Both
    /// have to be on the request or the answer cannot be attributed.
    #[test]
    fn a_request_carries_both_identities_and_a_window() {
        let registry = ApprovalRegistry::new();
        let request = registry.register("s1", "t1", "c1", &write_ask()).request;

        assert_eq!(request.session_id, "s1");
        assert_eq!(request.turn_id, "t1");
        assert_eq!(request.call_id, "c1");
        assert!(!request.request_id.is_empty());
        assert!(
            request.expires_at > request.requested_at,
            "{} is not after {}",
            request.expires_at,
            request.requested_at
        );
        assert!(request.session_grant_allowed);
        assert_eq!(request.scope_label, Grant::FsWrite.scope_label());
    }
}
