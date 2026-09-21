//! Parking an ask nobody can answer (PLAN 7.22).
//!
//! [`store::parked`](crate::store::parked) is the document; this is what the
//! turn does with it.
//!
//! * **Park, do not refuse.** An unattended run that meets a call outside what
//!   was signed keeps what it has written, the question is filed, and the
//!   model is told to return `blocked` naming it.
//! * **An expired dialog parks too**, so five minutes away from the machine no
//!   longer throws away the expensive part (`IDEAS.md` § 5).
//! * **Bounded**: [`MAX_PARKS_PER_RUN`] per run. Past it the call is refused
//!   the way it was before this section, because a run that cannot get through
//!   the gate three times is not going to on the fourth.
//! * **The notification says the routine and one sentence**, never the
//!   arguments: the dialog is in the window, not on a lock screen.

use crate::agent::event::{Event, EventSink};
use crate::approval::Decision;
use crate::notify::{Note, Notifier};
use crate::policy::AskRequest;
use crate::store::parked::{ParkCause, ParkDraft, ParkedAsk, ParkedStore, MAX_PARKS_PER_RUN};

/// What one run has parked, lent to the turn by whoever started it — the same
/// shape as [`skills::Reported`](crate::skills::Reported).
///
/// The scheduler reads it to record [`RunOutcome::Parked`], which is an
/// answer, not a silence.
///
/// [`RunOutcome::Parked`]: crate::store::RunOutcome::Parked
#[derive(Debug, Default)]
pub struct Parks {
    ids: std::sync::Mutex<Vec<String>>,
}

impl Parks {
    /// A cell with nothing in it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that this run parked a call.
    pub fn add(&self, id: &str) {
        self.lock().push(id.to_owned());
    }

    /// The ids parked so far, in the order they were parked.
    pub fn ids(&self) -> Vec<String> {
        self.lock().clone()
    }

    /// Whether this run parked anything.
    pub fn any(&self) -> bool {
        !self.lock().is_empty()
    }

    /// Locks the cell, recovering from a poisoned mutex.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Everything a turn needs to park a call, lent to it for the length of the
/// run.
pub struct Parking<'a> {
    /// Where parks are filed.
    pub store: &'a ParkedStore,
    /// Who is told that something is waiting.
    pub notifier: &'a dyn Notifier,
    /// The project whose board will show it.
    pub project_id: &'a str,
    /// The routine this run fires; empty for a session someone opened.
    pub routine_id: &'a str,
    /// Its name, or empty when no routine started this.
    pub routine_name: &'a str,
    /// The runbook on the clock; empty outside one.
    pub skill: &'a str,
    /// What this run has parked.
    pub parks: &'a Parks,
}

impl std::fmt::Debug for Parking<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parking")
            .field("project_id", &self.project_id)
            .field("routine_id", &self.routine_id)
            .finish_non_exhaustive()
    }
}

/// Which call is being parked, and why.
#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    /// The session whose run asked.
    pub session_id: &'a str,
    /// The identity it is made as.
    pub agent_id: &'a str,
    /// The turn.
    pub turn_id: &'a str,
    /// The model's own id for the call.
    pub call_id: &'a str,
    /// `<tool>:<args digest>` ([`fingerprint`](crate::audit::fingerprint)).
    pub fingerprint: &'a str,
    /// Whether nobody was there, or nobody answered.
    pub cause: ParkCause,
}

/// What became of the attempt to park.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// It is on the board, and the model is told so.
    Held(Box<ParkedAsk>),
    /// It could not be parked — the budget is spent, or the document would not
    /// write. The model gets an ordinary refusal, with this reason.
    Refused(String),
}

/// Files one park, tells whoever is not at the window, and answers the turn.
///
/// Never fails the call: a store that will not write is a refusal carrying the
/// wording this run would have had before parking existed.
pub fn park(
    parking: &Parking<'_>,
    call: Call<'_>,
    request: &AskRequest,
    sink: &dyn EventSink,
) -> Outcome {
    let draft = ParkDraft {
        project_id: parking.project_id,
        session_id: call.session_id,
        agent_id: call.agent_id,
        routine_id: parking.routine_id,
        routine_name: parking.routine_name,
        skill: parking.skill,
        turn_id: call.turn_id,
        call_id: call.call_id,
        fingerprint: call.fingerprint.to_owned(),
        cause: call.cause,
        request,
    };

    let ask = match parking.store.park(&draft) {
        Ok(ask) => ask,
        Err(err) => {
            tracing::info!(%err, tool = %request.tool, "a call could not be parked");
            return Outcome::Refused(spent(request, call.cause));
        }
    };

    parking.parks.add(&ask.id);
    parking.notifier.post(note(&ask));
    sink.emit(Event::ParkedUpdated(Box::new(ask.clone())));

    Outcome::Held(Box::new(ask))
}

/// What the model is told when its call is parked: that nothing ran, why, and
/// what to do with the rest of the run.
///
/// It names the park so a `blocked` return can quote it, and it never suggests
/// a way around the gate.
pub fn envelope(ask: &ParkedAsk) -> String {
    let opening = match ask.cause {
        ParkCause::Unattended => {
            "nobody is watching this run, and this call is outside what was signed on the routine"
        }
        ParkCause::Expired => "nobody answered the approval for this call within five minutes",
    };

    format!(
        "{opening}, so it is parked rather than refused: nothing ran, and the question is now \
         waiting for a person ({}). Keep what you have already done, do what you can without \
         this call, and finish with `skill_return` `blocked` naming what is parked: \"{}\". Do \
         not look for another way round it — if it is allowed, this run is resumed with the \
         answer.",
        ask.summary, ask.title
    )
}

/// What an unattended call is told when there is nowhere to park it: the
/// wording of Phase 16, kept because it is still the true one — no question
/// reached a person.
pub fn unsigned(request: &AskRequest) -> String {
    match &request.grant {
        // The routine could have been signed for this.
        Some(_) => format!(
            "nobody is watching this run, and this routine was not signed for `{}` ({}). Do what \
             you can without it, then return `blocked` and say what you needed",
            request.tool, request.summary
        ),
        // Nothing can be signed for this in advance (PLAN 3.1), so the model
        // should stop looking for a way through.
        None => format!(
            "nobody is watching this run, and `{}` here is put to a person every time it is \
             asked ({}), which no routine can be signed for in advance. Return `blocked` and say \
             what you needed",
            request.tool, request.reason
        ),
    }
}

/// The refusal a call gets when this run has already parked as many calls as
/// one run may.
fn spent(request: &AskRequest, cause: ParkCause) -> String {
    match cause {
        ParkCause::Expired => "nobody answered the approval for this call, so it was refused \
                               after five minutes. This turn has also already parked as many \
                               calls as one run may"
            .to_owned(),
        ParkCause::Unattended => format!(
            "{}. This run has also already parked {MAX_PARKS_PER_RUN} calls for a person, which \
             is as many as one run may",
            unsigned(request)
        ),
    }
}

/// The notification a park raises.
///
/// The routine's name and one sentence. The tool is named because a tool is
/// not an argument; the summary is not, because it is the path, the command
/// line or the amount (PLAN 7.22, *Refuses*).
fn note(ask: &ParkedAsk) -> Note {
    let key = if ask.routine_id.is_empty() {
        ask.session_id.clone()
    } else {
        ask.routine_id.clone()
    };
    let title = if ask.routine_name.is_empty() {
        "Aegis is waiting for you".to_owned()
    } else {
        ask.routine_name.clone()
    };

    Note::new(
        key,
        title,
        format!(
            "A `{}` call is parked for you to answer. Open Aegis to read it.",
            ask.tool
        ),
    )
}

/// How a resumed run's opening message begins; the scripted provider and the
/// tests match on it, the way they match on
/// [`OPENING_MARKER`](crate::schedule::OPENING_MARKER).
pub const RESUMED_MARKER: &str = "A person has answered";

/// The clause a refused answer carries, matched by the scripted provider for
/// the same reason as [`RESUMED_MARKER`]. A refusal is not a puzzle: the run
/// closes rather than asking the same thing a second way.
pub const REFUSED_CLAUSE: &str = "it is refused";

/// The sentence a resumed run opens with (PLAN 7.22).
///
/// One line naming the answer, and which runbook it is inside. The call itself
/// is in the transcript the run already has.
pub fn resumption(ask: &ParkedAsk, decision: Decision) -> String {
    let answer = match decision {
        Decision::AllowOnce => format!(
            "{RESUMED_MARKER} the `{}` call this run parked (\"{}\"): it is allowed this once, \
             with exactly the arguments you used. Make that call again now, unchanged.",
            ask.tool, ask.summary
        ),
        Decision::AllowSession => format!(
            "{RESUMED_MARKER} the `{}` call this run parked (\"{}\"): it is now a standing \
             approval, so it and what it covers no longer ask. Make the call again.",
            ask.tool, ask.summary
        ),
        Decision::Deny => format!(
            "{RESUMED_MARKER} the `{}` call this run parked (\"{}\"): {REFUSED_CLAUSE}. Do not \
             repeat it, and do not look for another way to do the same thing.",
            ask.tool, ask.summary
        ),
    };

    let carry = if ask.skill.is_empty() {
        "Carry on from where this stopped, and say what you did.".to_owned()
    } else {
        format!(
            "You are inside `skill:{}`: carry on with that runbook from where it stopped, and \
             finish with `skill_return`.",
            ask.skill
        )
    };

    format!("{answer} {carry}")
}

/// Whether an answer may only be recorded once its run can be picked up
/// (PLAN 7.22).
///
/// An **allow** may not: the call happens only if the run is resumed, so an
/// approval recorded while the run cannot be started would read as given while
/// nothing ran. It claims the run first and is refused when that fails.
///
/// A **refusal** is a fact about the person's decision, and the run it belongs
/// to has already ended — nothing is waiting on a channel, and the call cannot
/// be made now whatever the run does next. Blocking it on a busy routine would
/// mean clearing a backlog of questions one model turn at a time, so it is
/// recorded either way and the run is picked up only if the slot is free.
pub const fn needs_the_run(decision: Decision) -> bool {
    match decision {
        Decision::AllowOnce | Decision::AllowSession => true,
        Decision::Deny => false,
    }
}

/// How a closed park is named on `parked:resolved`, and in the ledger line of
/// a run that expired.
pub const fn answer_word(decision: Decision) -> &'static str {
    match decision {
        Decision::AllowOnce => "allow_once",
        Decision::AllowSession => "allow_standing",
        Decision::Deny => "deny",
    }
}

#[cfg(test)]
mod tests;
