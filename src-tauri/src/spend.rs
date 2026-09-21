//! Model spend, metered per round (PLAN 7.26, landing 1).
//!
//! A [`Meter`] is built for one turn with the price of the model it sends and
//! the caps of whoever it runs for. The turn asks it twice:
//!
//! * **before the first request** ([`Meter::refusal`]): a cap already reached,
//!   or a cap on a model with no price, and nothing is sent;
//! * **after every round** ([`Meter::charge`]): the round is priced, written to
//!   the [`SpendLedger`], and a cap it passed halts the turn with one wrap-up
//!   round, like the ceiling of PLAN 7.16.
//!
//! A round with no usage reported is estimated high on purpose: its request
//! size over [`BYTES_PER_TOKEN`], and the row's output ceiling.

use crate::agent::provider::Provider;
use crate::agent::wire::{ModelRequest, Usage, WireMessage};
use crate::notify::{Note, Notifier};
use crate::store::ledger::{self, dollars, Account, Micros, Scope};
use crate::store::{Agent, Binding, ModelPrice, Routine, SpendLedger};

/// What a round with no usage reported is assumed to have produced, when the
/// row does not know its model's ceiling.
pub const UNKNOWN_OUTPUT_TOKENS: u64 = 32_000;

/// Bytes per prompt token for an estimate. Real text runs nearer four; three
/// counts more tokens than were sent, which is the side to err on.
pub const BYTES_PER_TOKEN: u64 = 3;

/// Tokens counted for each image on an estimated request.
pub const IMAGE_TOKENS: u64 = 2_000;

/// The share of a day cap at which a person is told, in percent.
pub const WARN_PERCENT: u64 = 80;

/// Who answers for an identity in a session, and what its model costs: one
/// binding resolved once (PLAN 7.19, PLAN 7.26).
pub type Answering<'a> = dyn Fn(&Agent, &str) -> (Box<dyn Provider>, Tariff) + Send + Sync + 'a;

/// What the model a turn sends costs: resolved from the same binding as its
/// provider (PLAN 7.19), so the price and the model cannot differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tariff {
    /// The provider row that answers.
    pub provider_id: String,
    /// The model it sends.
    pub model: String,
    /// Its price on that row, if the operator entered one.
    pub price: Option<ModelPrice>,
    /// The row's output ceiling for that model, when known.
    pub max_output_tokens: Option<u32>,
}

impl Tariff {
    /// The tariff of a resolved binding.
    pub fn of(binding: &Binding) -> Self {
        let model = binding.settings.model.clone();
        Self {
            provider_id: binding.provider_id.clone(),
            price: binding.settings.price_of(&model).cloned(),
            max_output_tokens: binding.settings.max_output_tokens,
            model,
        }
    }

    /// A model with no price on the default row: what a test's scripted
    /// provider costs.
    pub fn unpriced(model: &str) -> Self {
        Self {
            provider_id: crate::store::DEFAULT_PROVIDER_ID.to_owned(),
            model: model.to_owned(),
            price: None,
            max_output_tokens: None,
        }
    }
}

/// Whose account a turn runs on.
#[derive(Debug, Clone, Copy)]
pub struct Payer<'a> {
    /// The project, or empty.
    pub project_id: &'a str,
    /// The session.
    pub session_id: &'a str,
    /// The turn.
    pub turn_id: &'a str,
    /// The identity it runs as.
    pub agent: &'a Agent,
    /// The routine whose run this is.
    pub routine: Option<&'a Routine>,
    /// Whether a run is the whole session (a routine's run, a brief) rather
    /// than this one turn (a session someone opened).
    pub run_is_session: bool,
}

/// Whose cap it is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    Routine { id: String, name: String },
    Identity { id: String, name: String },
}

impl Owner {
    fn label(&self) -> String {
        match self {
            Self::Routine { name, .. } => format!("the routine `{name}`"),
            Self::Identity { name, .. } => format!("the identity `{name}`"),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Routine { name, .. } | Self::Identity { name, .. } => name,
        }
    }

    fn id(&self) -> &str {
        match self {
            Self::Routine { id, .. } | Self::Identity { id, .. } => id,
        }
    }
}

/// What a cap covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Span {
    Run,
    Day,
}

/// One cap in force for this turn.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cap {
    owner: Owner,
    span: Span,
    limit: Micros,
}

impl Cap {
    fn describe(&self, spent: Micros) -> String {
        let span = match self.span {
            Span::Run => "per-run",
            Span::Day => "daily",
        };
        format!(
            "{}'s {span} model budget of {} is spent ({} so far)",
            self.owner.label(),
            dollars(self.limit),
            dollars(spent)
        )
    }
}

/// The meter for one turn.
pub struct Meter<'a> {
    ledger: &'a SpendLedger,
    notifier: &'a dyn Notifier,
    tariff: Tariff,
    project_id: String,
    session_id: String,
    turn_id: String,
    agent_id: String,
    routine_id: String,
    run_is_session: bool,
    day: String,
    caps: Vec<Cap>,
}

impl<'a> Meter<'a> {
    /// A meter for `payer`'s turn at `tariff`, under the caps of its routine
    /// and its identity.
    pub fn new(
        ledger: &'a SpendLedger,
        notifier: &'a dyn Notifier,
        tariff: Tariff,
        payer: Payer<'_>,
    ) -> Self {
        let mut caps = Vec::new();
        let mut push = |owner: Owner, caps_of: crate::store::SpendCaps| {
            if let Some(limit) = caps_of.per_run {
                caps.push(Cap {
                    owner: owner.clone(),
                    span: Span::Run,
                    limit,
                });
            }
            if let Some(limit) = caps_of.per_day {
                caps.push(Cap {
                    owner,
                    span: Span::Day,
                    limit,
                });
            }
        };
        if let Some(routine) = payer.routine {
            push(
                Owner::Routine {
                    id: routine.id.clone(),
                    name: routine.name.clone(),
                },
                routine.spend,
            );
        }
        push(
            Owner::Identity {
                id: payer.agent.id.clone(),
                name: payer.agent.name.clone(),
            },
            payer.agent.spend,
        );

        Self {
            ledger,
            notifier,
            tariff,
            project_id: payer.project_id.to_owned(),
            session_id: payer.session_id.to_owned(),
            turn_id: payer.turn_id.to_owned(),
            agent_id: payer.agent.id.clone(),
            routine_id: payer.routine.map(|r| r.id.clone()).unwrap_or_default(),
            run_is_session: payer.run_is_session,
            day: ledger::today(),
            caps,
        }
    }

    /// Whether this meter prices anything: without a price nothing is
    /// recorded, and [`Meter::refusal`] has already stopped a capped turn.
    pub const fn priced(&self) -> bool {
        self.tariff.price.is_some()
    }

    /// What this turn has cost so far, when its model has a price.
    pub fn turn_spent(&self) -> Option<Micros> {
        self.priced()
            .then(|| self.ledger.spent(Scope::Turn(&self.turn_id)))
    }

    /// Why this turn must not send its first request, or `None`.
    pub fn refusal(&self) -> Option<String> {
        let first = self.caps.first()?;
        if self.tariff.price.is_none() {
            return Some(format!(
                "`{}` has no price on its provider row, and {} has a model-spend cap that cannot \
                 be measured without one. Add the model's price under Settings → Providers",
                self.tariff.model,
                first.owner.label()
            ));
        }
        self.reached()
    }

    /// Prices one round, writes it to the ledger, and says which cap it
    /// reached, if any. `estimate` is the request's prompt estimate
    /// ([`prompt_estimate`]), read only when `usage` is `None`.
    pub fn charge(&self, usage: Option<&Usage>, estimate: u64) -> Option<String> {
        let price = self.tariff.price.as_ref()?;

        let (micros, estimated) = match usage {
            Some(used) => (
                price.cost(
                    used.prompt_tokens,
                    used.cache_read_tokens,
                    used.cache_creation_tokens,
                    used.completion_tokens,
                ),
                false,
            ),
            None => {
                let output = self
                    .tariff
                    .max_output_tokens
                    .map_or(UNKNOWN_OUTPUT_TOKENS, u64::from);
                (price.cost(estimate, 0, 0, output), true)
            }
        };

        let before: Vec<Micros> = self.caps.iter().map(|cap| self.spent(cap)).collect();

        let account = Account {
            turn_id: &self.turn_id,
            session_id: &self.session_id,
            project_id: &self.project_id,
            agent_id: &self.agent_id,
            routine_id: &self.routine_id,
            provider_id: &self.tariff.provider_id,
            model: &self.tariff.model,
        };
        if let Err(err) = self.ledger.charge(&account, micros, estimated) {
            // Counted in memory all the same, so enforcement holds.
            tracing::warn!(%err, turn_id = %self.turn_id, "a round's spend could not be saved");
        }

        for (cap, was) in self.caps.iter().zip(before) {
            if cap.span == Span::Day {
                self.warn(cap, was, was.saturating_add(micros));
            }
        }
        self.reached()
    }

    /// The first cap already at or past its limit, described.
    fn reached(&self) -> Option<String> {
        self.caps.iter().find_map(|cap| {
            let spent = self.spent(cap);
            (spent >= cap.limit).then(|| cap.describe(spent))
        })
    }

    /// What `cap`'s scope has spent so far.
    fn spent(&self, cap: &Cap) -> Micros {
        let scope = match (&cap.owner, cap.span) {
            (_, Span::Run) if self.run_is_session => Scope::Session(&self.session_id),
            (_, Span::Run) => Scope::Turn(&self.turn_id),
            (Owner::Routine { id, .. }, Span::Day) => Scope::RoutineDay(id, &self.day),
            (Owner::Identity { id, .. }, Span::Day) => Scope::AgentDay(id, &self.day),
        };
        self.ledger.spent(scope)
    }

    /// Tells a person when a day cap crosses [`WARN_PERCENT`] or its limit. The
    /// ledger only grows within a day, so each crossing happens once.
    fn warn(&self, cap: &Cap, was: Micros, now: Micros) {
        let warn_at = cap.limit.saturating_mul(WARN_PERCENT) / 100;
        let (threshold, body) = if was < cap.limit && now >= cap.limit {
            (
                "100",
                "has reached its model budget for today; its turns stop until the budget resets \
                 at midnight UTC."
                    .to_owned(),
            )
        } else if was < warn_at && now >= warn_at {
            (
                "80",
                format!("has used {WARN_PERCENT} % of its model budget for today."),
            )
        } else {
            return;
        };

        tracing::info!(
            owner = cap.owner.name(),
            threshold,
            "a daily model budget crossed a threshold"
        );
        self.notifier.post(Note::new(
            format!("spend:{}:{}:{threshold}", cap.owner.id(), self.day),
            cap.owner.name(),
            body,
        ));
    }
}

/// The prompt tokens a request is assumed to hold when the provider reports
/// none: its body over [`BYTES_PER_TOKEN`], plus [`IMAGE_TOKENS`] an image.
pub fn prompt_estimate(request: &ModelRequest) -> u64 {
    let bytes = u64::try_from(request.to_body().to_string().len()).unwrap_or(u64::MAX);
    let images = request
        .messages
        .iter()
        .map(|message| match message {
            WireMessage::User { images, .. } | WireMessage::Tool { images, .. } => images.len(),
            WireMessage::System { .. } | WireMessage::Assistant { .. } => 0,
        })
        .sum::<usize>();
    let images = u64::try_from(images).unwrap_or(u64::MAX);
    (bytes / BYTES_PER_TOKEN).saturating_add(images.saturating_mul(IMAGE_TOKENS))
}

#[cfg(test)]
mod tests;
