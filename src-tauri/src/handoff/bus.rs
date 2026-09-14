//! The handoff bus: fan-out, fan-in, and what happens when nobody comes back
//! (PLAN 7.3, Phase 15; `COS.md` *Loop*).
//!
//! ```text
//!                  ┌── brief ──▶ owner A ──┐
//!  handoff_delegate┤                       ├──▶ board ──▶ reviewer ──▶ CoS
//!                  └── brief ──▶ owner B ──┘
//! ```
//!
//! The policy of a delegation; [`Runner`] is the machinery (the app's
//! [`runner`](super::runner), or a script in tests).
//!
//! * **Parallel**: one task and one session per brief; results keep brief
//!   order.
//! * **Bounded**: each attempt runs under [`ATTEMPT_TIMEOUT`], cancelled
//!   through its Stop token.
//! * **[`ATTEMPTS`] attempts, then the human**: the retry continues the same
//!   session.
//! * **Failure is an answer**: an escalation is a `needs_you` line, not an
//!   error.
//!
//! The CoS reads back only [`Board::render`] — statuses, paths, questions —
//! since a [`Report`] has no field that could hold a transcript.

use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

use super::{Brief, Report, Status};

/// How many briefs one delegation may carry: what a person can review in one
/// dialog.
pub const FAN_OUT_MAX: usize = 4;

/// How long one attempt at a brief may take before it is cancelled.
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(300);

/// How many attempts a brief gets before the human is asked (`COS.md` *Loop*).
pub const ATTEMPTS: u32 = 2;

/// Why one attempt produced no report.
///
/// `retryable` is false when a retry would fail identically (e.g. no such
/// identity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// What went wrong, in a line, written to be read on the board.
    pub reason: String,
    /// Whether trying again could plausibly do better.
    pub retryable: bool,
}

impl Failure {
    /// A failure worth one more attempt.
    pub fn retryable(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: true,
        }
    }

    /// A failure that would happen again the same way.
    pub fn fatal(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: false,
        }
    }
}

/// One attempt, in flight.
pub type Running<'a> = Pin<Box<dyn Future<Output = Result<Report, Failure>> + Send + 'a>>;

/// Which brief of which delegation, and which try at it.
///
/// Lets a retry find its session. `seq` is the brief's position; the reviewer's
/// is one past the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot<'a> {
    /// The delegation. One id over the CoS and everyone under it.
    pub handoff: &'a str,
    /// Which brief of the fan-out, from zero.
    pub seq: usize,
    /// Which try, from one. Zero while the brief is only being filed.
    pub attempt: u32,
}

/// What the bus needs from the runtime in order to carry a brief.
///
/// File the brief, then run it — nothing else reaches this module.
pub trait Runner: Send + Sync {
    /// Writes the brief where the team can read it, and says where it went.
    ///
    /// Called once per brief before any attempt (`slot.attempt` is zero).
    /// `None` without `.aegis/briefs/`, which is never created; the brief then
    /// travels in the session.
    fn file(&self, slot: Slot<'_>, brief: &Brief, rendered: &str) -> Option<String>;

    /// Runs one brief as its owner, and reports what came back.
    ///
    /// `attempt` is 1-based; the second attempt is told it is one.
    fn run<'a>(
        &'a self,
        slot: Slot<'a>,
        brief: &'a Brief,
        filed: Option<&'a str>,
        cancel: &'a CancellationToken,
    ) -> Running<'a>;
}

/// How one brief ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The owner returned, and the return passed [`check`](super::check).
    Returned(Report),
    /// Nobody returned; the human is the next owner. A status, not an error.
    Escalated {
        /// What the last attempt said went wrong.
        reason: String,
    },
}

impl Outcome {
    /// The status word on the board; an escalation reads `needs_you`.
    pub const fn status(&self) -> Status {
        match self {
            Self::Returned(report) => report.status,
            Self::Escalated { .. } => Status::NeedsYou,
        }
    }
}

/// One brief and what came of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The brief as it went out.
    pub brief: Brief,
    /// Where it was filed, when there was a `.aegis/briefs/` to file it in.
    pub filed: Option<String>,
    /// How many attempts it took, or how many it survived.
    pub attempts: u32,
    /// What came back.
    pub outcome: Outcome,
}

/// Everything one delegation produced, with the reviewer kept apart from the
/// specialists (`COS.md` *Loop*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    /// This delegation's id, on every audit line of the CoS call and its runs
    /// (PLAN 7.2, row 10).
    pub id: String,
    /// One per brief, in the order they were written.
    pub assignments: Vec<Assignment>,
    /// The reviewer's own return, when one was asked for.
    pub review: Option<Assignment>,
}

impl Board {
    /// How many owners returned a `done`.
    pub fn done(&self) -> usize {
        self.count(Status::Done)
    }

    /// How many are blocked.
    pub fn blocked(&self) -> usize {
        self.count(Status::Blocked)
    }

    /// How many need a person — returned `needs_you`, or never returned.
    pub fn needs_you(&self) -> usize {
        self.count(Status::NeedsYou)
    }

    /// Specialists at one status. The reviewer is not counted: it is a verdict
    /// on the work, not another piece of it.
    fn count(&self, status: Status) -> usize {
        self.assignments
            .iter()
            .filter(|assignment| assignment.outcome.status() == status)
            .count()
    }

    /// One line summarizing the whole delegation.
    pub fn headline(&self) -> String {
        let mut parts = Vec::new();
        for (count, word) in [
            (self.done(), "done"),
            (self.blocked(), "blocked"),
            (self.needs_you(), "needs you"),
        ] {
            if count > 0 {
                parts.push(format!("{count} {word}"));
            }
        }

        let owners = match self.assignments.len() {
            1 => "1 brief".to_owned(),
            n => format!("{n} briefs"),
        };
        let mut line = format!("{owners}: {}", parts.join(", "));
        if let Some(review) = &self.review {
            let _ = write!(line, "; review {}", review.outcome.status().as_str());
        }
        line
    }

    /// What goes back to the CoS: statuses, paths and questions from each
    /// [`Report`].
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}\n", self.headline());

        for (index, assignment) in self.assignments.iter().enumerate() {
            let _ = write!(
                out,
                "{}",
                assignment.render(&format!("brief {}", index + 1))
            );
            out.push('\n');
        }

        if let Some(review) = &self.review {
            let _ = write!(out, "{}", review.render("review"));
        }

        out
    }
}

impl Assignment {
    /// Whether an owner actually returned something.
    const fn returned(&self) -> bool {
        matches!(self.outcome, Outcome::Returned(_))
    }

    /// This assignment as a block of the board.
    fn render(&self, label: &str) -> String {
        let mut out = String::new();

        let _ = writeln!(
            out,
            "--- {label} — {} ({})",
            self.brief.goal.trim(),
            self.brief.owner.trim()
        );
        if let Some(filed) = &self.filed {
            let _ = writeln!(out, "brief: {filed}");
        }

        match &self.outcome {
            Outcome::Returned(report) => {
                let _ = write!(out, "{}", super::render(report, report.summary.trim()));
            }
            Outcome::Escalated { reason } => {
                let _ = writeln!(out, "status: {}", Status::NeedsYou.as_str());
                let _ = writeln!(
                    out,
                    "summary:\n  no return after {} attempts: {reason}",
                    self.attempts
                );
                let _ = writeln!(out, "artefacts: —");
                let _ = writeln!(out, "evidence: —");
                let _ = writeln!(
                    out,
                    "open_questions:\n  - should this be re-briefed, given differently, or dropped?"
                );
                let _ = writeln!(out, "next_owner: the human");
            }
        }

        out
    }
}

/// Carries one delegation from end to end.
///
/// Files every brief, runs them together, then any review. Never fails: each
/// problem is a board line.
pub async fn deliver(
    runner: &Arc<dyn Runner>,
    plan: super::Plan,
    cancel: &CancellationToken,
) -> Board {
    let id = uuid::Uuid::new_v4().to_string();
    let super::Plan { briefs, review } = plan;

    let filed: Vec<Option<String>> = briefs
        .iter()
        .enumerate()
        .map(|(seq, brief)| {
            super::check_brief(brief).ok().and_then(|rendered| {
                runner.file(
                    Slot {
                        handoff: &id,
                        seq,
                        attempt: 0,
                    },
                    brief,
                    &rendered,
                )
            })
        })
        .collect();

    // Spawned, so briefs overlap and a panic fails one assignment, not the CoS.
    let count = briefs.len();
    let mut running = Vec::with_capacity(count);
    for (seq, (brief, filed)) in briefs.into_iter().zip(filed).enumerate() {
        let runner = Arc::clone(runner);
        let cancel = cancel.clone();
        let id = id.clone();

        running.push(tokio::spawn(async move {
            attempt(runner.as_ref(), &id, seq, brief, filed, &cancel).await
        }));
    }

    let mut assignments = Vec::with_capacity(running.len());
    for task in running {
        match task.await {
            Ok(assignment) => assignments.push(assignment),
            // The task itself came apart. There is no brief to report against
            // — it went into the task — so this is logged and dropped rather
            // than guessed at. The board is short by one, which is visible.
            Err(err) => tracing::error!(%err, "a delegated run did not come back"),
        }
    }

    let review = match review {
        // Nothing came back, so there is nothing to review. Sending a reviewer
        // an empty board would spend a run to be told what the board already
        // says.
        Some(_) if !assignments.iter().any(|a| a.returned()) => {
            tracing::info!("no owner returned; the review was not run");
            None
        }
        Some(mut brief) => {
            fan_in(&mut brief, &assignments);
            let slot = Slot {
                handoff: &id,
                seq: count,
                attempt: 0,
            };
            let rendered = super::check_brief(&brief).ok();
            let filed = rendered.and_then(|rendered| runner.file(slot, &brief, &rendered));
            Some(attempt(runner.as_ref(), &id, count, brief, filed, cancel).await)
        }
        None => None,
    };

    Board {
        id,
        assignments,
        review,
    }
}

/// Points the reviewer at what came back, and at nothing else.
///
/// The fan-in: the reviewer's inputs are the named artefacts and the briefs,
/// never the sessions (`COS.md` *Loop*).
fn fan_in(brief: &mut Brief, assignments: &[Assignment]) {
    for assignment in assignments {
        if let Outcome::Returned(report) = &assignment.outcome {
            for artefact in &report.artefacts {
                if !brief.inputs.contains(&artefact.shown) {
                    brief.inputs.push(artefact.shown.clone());
                }
            }
        }
    }

    // Capped here, so the fan-in never builds a brief `check_brief` refuses.
    brief.inputs.truncate(super::BRIEF_LIST_MAX);
}

/// Runs one brief until it returns, or until the attempts run out.
async fn attempt(
    runner: &dyn Runner,
    id: &str,
    seq: usize,
    brief: Brief,
    filed: Option<String>,
    cancel: &CancellationToken,
) -> Assignment {
    let mut last = String::from("nothing was attempted");

    for attempt in 1..=ATTEMPTS {
        if cancel.is_cancelled() {
            last = "the delegation was cancelled".to_owned();
            return Assignment {
                brief,
                filed,
                attempts: attempt.saturating_sub(1),
                outcome: Outcome::Escalated { reason: last },
            };
        }

        // The attempt's own token, so a timeout stops one owner rather than the
        // whole fan-out. It is a child of the delegation's: a Stop on the CoS
        // turn still reaches every specialist under it.
        let own = cancel.child_token();
        let outcome = timeout(
            ATTEMPT_TIMEOUT,
            runner.run(
                Slot {
                    handoff: id,
                    seq,
                    attempt,
                },
                &brief,
                filed.as_deref(),
                &own,
            ),
        )
        .await;

        match outcome {
            Ok(Ok(report)) => {
                return Assignment {
                    brief,
                    filed,
                    attempts: attempt,
                    outcome: Outcome::Returned(report),
                }
            }
            Ok(Err(failure)) => {
                tracing::info!(
                    owner = %brief.owner,
                    attempt,
                    reason = %failure.reason,
                    "a delegated run did not return"
                );
                last = failure.reason;
                if !failure.retryable {
                    break;
                }
            }
            Err(_elapsed) => {
                // Cancel rather than drop: the run is a task with a session and
                // possibly a child process behind it, and letting the future go
                // would leave both to finish work nobody is waiting for.
                own.cancel();
                last = format!(
                    "it did not return within {} seconds",
                    ATTEMPT_TIMEOUT.as_secs()
                );
                tracing::info!(owner = %brief.owner, attempt, "a delegated run timed out");
            }
        }
    }

    Assignment {
        brief,
        filed,
        attempts: ATTEMPTS,
        outcome: Outcome::Escalated { reason: last },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::handoff::{Priority, ReturnFormat};

    /// A runner driven by a script: one entry per attempt, in order.
    struct Script {
        answers: Mutex<Vec<Result<Report, Failure>>>,
        seen: Mutex<Vec<(String, u32)>>,
        stall: bool,
    }

    impl Script {
        fn arc(answers: Vec<Result<Report, Failure>>) -> Arc<dyn Runner> {
            Arc::new(Self {
                answers: Mutex::new(answers),
                seen: Mutex::new(Vec::new()),
                stall: false,
            })
        }
    }

    impl Runner for Script {
        fn file(&self, _slot: Slot<'_>, _brief: &Brief, _rendered: &str) -> Option<String> {
            Some(".aegis/briefs/filed.md".to_owned())
        }

        fn run<'a>(
            &'a self,
            slot: Slot<'a>,
            brief: &'a Brief,
            _filed: Option<&'a str>,
            cancel: &'a CancellationToken,
        ) -> Running<'a> {
            Box::pin(async move {
                self.seen
                    .lock()
                    .expect("seen")
                    .push((brief.owner.clone(), slot.attempt));

                if self.stall {
                    cancel.cancelled().await;
                    return Err(Failure::retryable("cancelled"));
                }

                let next = self.answers.lock().expect("answers").pop();
                next.unwrap_or_else(|| Err(Failure::retryable("the script ran out")))
            })
        }
    }

    fn brief(owner: &str) -> Brief {
        Brief {
            goal: format!("do the {owner} thing"),
            owner: owner.to_owned(),
            priority: Priority::Normal,
            inputs: vec![".aegis/briefs/intake.md".to_owned()],
            constraints: Vec::new(),
            definition_of_done: "the file exists".to_owned(),
            approval_needed: String::new(),
            return_format: ReturnFormat::Status,
        }
    }

    fn report(status: Status) -> Report {
        Report {
            status,
            summary: "did it".to_owned(),
            artefacts: Vec::new(),
            evidence: vec!["looked".to_owned()],
            open_questions: match status {
                Status::Done => Vec::new(),
                _ => vec!["what now?".to_owned()],
            },
            next_owner: String::new(),
        }
    }

    /// PLAN 7.3, Phase 15, the exit criterion: two briefs, one wait, and a
    /// board rather than a concatenated transcript.
    #[tokio::test]
    async fn two_briefs_come_back_as_one_board_in_the_order_they_were_written() {
        let runner = Script::arc(vec![Ok(report(Status::Blocked)), Ok(report(Status::Done))]);
        let board = deliver(
            &runner,
            crate::handoff::Plan {
                briefs: vec![brief("Scribe"), brief("Reviewer")],
                review: None,
            },
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(board.assignments.len(), 2);
        assert_eq!(board.assignments[0].brief.owner, "Scribe");
        assert_eq!(board.assignments[1].brief.owner, "Reviewer");
        assert_eq!(board.done(), 1);
        assert_eq!(board.blocked(), 1);

        let rendered = board.render();
        assert!(rendered.contains("2 briefs"), "{rendered}");
        assert!(rendered.contains("status: done"), "{rendered}");
        assert!(rendered.contains("status: blocked"), "{rendered}");
    }

    /// `COS.md` *Loop*: escalate after two failures, not twelve.
    #[tokio::test]
    async fn a_brief_that_fails_twice_goes_to_the_human_and_no_further() {
        let runner = Script::arc(vec![
            Err(Failure::retryable("second")),
            Err(Failure::retryable("first")),
        ]);
        let board = deliver(
            &runner,
            crate::handoff::Plan {
                briefs: vec![brief("Scribe")],
                review: None,
            },
            &CancellationToken::new(),
        )
        .await;

        let assignment = &board.assignments[0];
        assert_eq!(assignment.attempts, ATTEMPTS);
        assert!(matches!(assignment.outcome, Outcome::Escalated { .. }));
        assert_eq!(assignment.outcome.status(), Status::NeedsYou);

        let rendered = board.render();
        assert!(rendered.contains("needs_you"), "{rendered}");
        assert!(rendered.contains("2 attempts"), "{rendered}");
        assert!(rendered.contains("the human"), "{rendered}");
    }

    #[tokio::test]
    async fn a_retryable_failure_is_tried_once_more_and_the_second_attempt_counts() {
        let runner = Script::arc(vec![
            Ok(report(Status::Done)),
            Err(Failure::retryable("hm")),
        ]);
        let board = deliver(
            &runner,
            crate::handoff::Plan {
                briefs: vec![brief("Scribe")],
                review: None,
            },
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(board.assignments[0].attempts, 2);
        assert_eq!(board.done(), 1);
    }

    /// A failure that would repeat identically costs one attempt, not two: the
    /// second would only make the user wait longer for the same escalation.
    #[tokio::test]
    async fn a_fatal_failure_is_not_retried() {
        let runner = Script::arc(vec![Err(Failure::fatal("no such identity"))]);
        let board = deliver(
            &runner,
            crate::handoff::Plan {
                briefs: vec![brief("Nobody")],
                review: None,
            },
            &CancellationToken::new(),
        )
        .await;

        assert!(board.render().contains("no such identity"));
    }

    #[tokio::test(start_paused = true)]
    async fn an_owner_that_never_answers_is_cancelled_rather_than_waited_on() {
        let runner: Arc<dyn Runner> = Arc::new(Script {
            answers: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
            stall: true,
        });

        let board = deliver(
            &runner,
            crate::handoff::Plan {
                briefs: vec![brief("Scribe")],
                review: None,
            },
            &CancellationToken::new(),
        )
        .await;

        assert!(matches!(
            board.assignments[0].outcome,
            Outcome::Escalated { .. }
        ));
        assert!(board.render().contains("did not return within"));
    }

    /// The fan-in: the reviewer is given the artefacts, and is not given
    /// anything else that came out of those runs.
    #[tokio::test]
    async fn the_reviewer_receives_the_artefacts_and_no_transcript() {
        let mut produced = report(Status::Done);
        produced.artefacts = vec![crate::handoff::Artefact {
            shown: ".aegis/artefacts/draft.md".to_owned(),
            path: std::path::PathBuf::from(".aegis/artefacts/draft.md"),
        }];

        let runner = Script::arc(vec![Ok(report(Status::Done)), Ok(produced)]);
        let board = deliver(
            &runner,
            crate::handoff::Plan {
                briefs: vec![brief("Scribe")],
                review: Some(brief("Reviewer")),
            },
            &CancellationToken::new(),
        )
        .await;

        let review = board.review.as_ref().expect("a review ran");
        assert!(
            review
                .brief
                .inputs
                .contains(&".aegis/artefacts/draft.md".to_owned()),
            "{:?}",
            review.brief.inputs
        );
        assert!(board.render().contains("--- review"));
    }

    /// Reviewing nothing is a run nobody needs: the board already says every
    /// owner escalated.
    #[tokio::test]
    async fn no_review_runs_when_nothing_came_back() {
        let runner = Script::arc(vec![
            Err(Failure::fatal("gone")),
            Err(Failure::fatal("gone")),
        ]);
        let board = deliver(
            &runner,
            crate::handoff::Plan {
                briefs: vec![brief("Scribe")],
                review: Some(brief("Reviewer")),
            },
            &CancellationToken::new(),
        )
        .await;

        assert!(board.review.is_none());
    }
}
