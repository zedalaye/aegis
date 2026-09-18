//! The decision client: TypeSafe's System One API (PLAN 7.18).
//!
//! Jev answers typed questions about a state with probabilities. It is not a
//! [`Provider`](crate::agent::Provider): it streams no text, calls no tool and
//! never answers a turn. Code owns the questions and composes the answers.
//!
//! * [`DecisionClient`] — one `POST {base}/v1/systemone`, bearer key, retries
//!   on 429 / 529. No TypeSafe JSON leaves this tree.
//! * [`tool_risk`] — the harness-owned eval that annotates an approval dialog.
//! * [`eval`] — signed project evals (`.aegis/evals/<name>/eval.yml`) and their
//!   closed composition vocabulary.

pub mod eval;
pub mod tool_risk;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent::ProviderProbe;
use crate::error::ErrorCode;
use crate::secrets::ApiKey;
use crate::store::DecisionSettings;

/// The path appended to the origin.
const ENDPOINT_PATH: &str = "/v1/systemone";

/// Most bytes of JSON a state may take (PLAN 7.18).
pub const STATE_MAX_BYTES: usize = 64 * 1024;

/// How long one request may take, answer included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the probe waits.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Waits before each retry of a 429 or a 529.
const BACKOFF: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1500)];

/// Most of an error body quoted back.
const ERROR_BODY_CHARS: usize = 300;

/// HTTP 529, TypeSafe's "overloaded".
const OVERLOADED: u16 = 529;

// ---------------------------------------------------------------------------
// Questions
// ---------------------------------------------------------------------------

/// Which kind of snap judgement a question asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionKind {
    /// A probability that the statement holds.
    Noul,
    /// One option out of several.
    Choice,
    /// A position on ordered levels.
    Score,
}

impl QuestionKind {
    /// The wire word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Noul => "noul",
            Self::Choice => "choice",
            Self::Score => "score",
        }
    }
}

/// What a question is judged against. Each value is a string or a structured
/// description (`what`, `not_for`, `examples`), kept as JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum Criteria {
    /// Optional descriptions of the true and false cases.
    Noul {
        /// When the answer is yes.
        yes: Option<Value>,
        /// When the answer is no.
        no: Option<Value>,
    },
    /// Two or more named options.
    Choice {
        /// Option name → description (`null` when the name says it all).
        options: Vec<(String, Value)>,
    },
    /// Two or more levels, lowest first.
    Score {
        /// The levels.
        levels: Vec<Value>,
    },
}

/// One atomic question, parsed and checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    /// The key its answer comes back under. Never inference input.
    pub id: String,
    /// What it asks. A string, an object or an array.
    pub instructions: Value,
    /// Its kind, with the criteria that kind needs.
    pub criteria: Criteria,
}

impl Question {
    /// The question's kind.
    pub const fn kind(&self) -> QuestionKind {
        match self.criteria {
            Criteria::Noul { .. } => QuestionKind::Noul,
            Criteria::Choice { .. } => QuestionKind::Choice,
            Criteria::Score { .. } => QuestionKind::Score,
        }
    }

    /// One line for a dialog: `id (kind): instructions`.
    pub fn line(&self) -> String {
        const HEAD: usize = 120;
        let text = match &self.instructions {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let head: String = text.chars().take(HEAD).collect();
        let ellipsis = if text.chars().count() > HEAD {
            "…"
        } else {
            ""
        };
        format!("{} ({}): {head}{ellipsis}", self.id, self.kind().as_str())
    }

    /// The option names of a choice, or nothing.
    pub fn options(&self) -> Vec<&str> {
        match &self.criteria {
            Criteria::Choice { options } => options.iter().map(|(name, _)| name.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    /// The TypeSafe shape of this question: `type`, `instructions`, `criteria`.
    fn to_wire(&self) -> Value {
        let mut out = json!({
            "type": self.kind().as_str(),
            "instructions": self.instructions,
        });
        let criteria = match &self.criteria {
            Criteria::Noul { yes, no } => {
                let mut map = Map::new();
                if let Some(yes) = yes {
                    map.insert("true".to_owned(), yes.clone());
                }
                if let Some(no) = no {
                    map.insert("false".to_owned(), no.clone());
                }
                (!map.is_empty()).then_some(Value::Object(map))
            }
            Criteria::Choice { options } => Some(Value::Object(
                options
                    .iter()
                    .map(|(name, description)| (name.clone(), description.clone()))
                    .collect(),
            )),
            Criteria::Score { levels } => Some(Value::Array(levels.clone())),
        };
        if let (Some(criteria), Some(object)) = (criteria, out.as_object_mut()) {
            object.insert("criteria".to_owned(), criteria);
        }
        out
    }
}

/// A question as written by a model or in an `eval.yml`, before checking.
///
/// `yes` / `no` / `options` / `levels` are the readable spellings; TypeSafe's
/// own `criteria` is accepted too, never both.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionDraft {
    /// The answer's key. Filled from the map key in an `eval.yml`.
    #[serde(default)]
    pub id: Option<String>,
    /// `noul`, `choice` or `score`.
    #[serde(rename = "type")]
    pub kind: QuestionKind,
    /// What it asks.
    pub instructions: Value,
    /// Noul: the true case.
    #[serde(default)]
    pub yes: Option<Value>,
    /// Noul: the false case.
    #[serde(default)]
    pub no: Option<Value>,
    /// Choice: option name → description.
    #[serde(default)]
    pub options: Option<Map<String, Value>>,
    /// Score: the levels, lowest first.
    #[serde(default)]
    pub levels: Option<Vec<Value>>,
    /// TypeSafe's own spelling of the above.
    #[serde(default)]
    pub criteria: Option<Value>,
}

/// Whether a question id is shaped like one: what a composition names it by.
pub fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && id.starts_with(|c: char| c.is_ascii_lowercase())
}

/// Whether a value is usable as instructions or a criterion.
fn is_text(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Array(items) => !items.is_empty(),
        _ => false,
    }
}

impl QuestionDraft {
    /// Checks the draft into a [`Question`]. Errors are written for whoever
    /// wrote it — a model or an eval's author.
    pub fn check(self, id: String) -> Result<Question, String> {
        if !is_id(&id) {
            return Err(format!(
                "`{id}` is not a question id. Use lower-case letters, digits and `_`, starting \
                 with a letter"
            ));
        }
        if !is_text(&self.instructions) {
            return Err(format!(
                "`{id}` has no instructions: a string, or an object describing what is judged"
            ));
        }
        let readable = self.yes.is_some()
            || self.no.is_some()
            || self.options.is_some()
            || self.levels.is_some();
        if readable && self.criteria.is_some() {
            return Err(format!(
                "`{id}` gives both `criteria` and `yes`/`no`/`options`/`levels`; use one"
            ));
        }

        let criteria = match self.kind {
            QuestionKind::Noul => {
                if self.options.is_some() || self.levels.is_some() {
                    return Err(format!(
                        "`{id}` is a noul: it takes `yes` and `no`, not options or levels"
                    ));
                }
                let (yes, no) = match self.criteria {
                    Some(Value::Object(map)) => {
                        (map.get("true").cloned(), map.get("false").cloned())
                    }
                    Some(_) => {
                        return Err(format!(
                            "`{id}`: a noul's `criteria` is an object with `true` and `false`"
                        ))
                    }
                    None => (self.yes, self.no),
                };
                Criteria::Noul { yes, no }
            }
            QuestionKind::Choice => {
                if self.yes.is_some() || self.no.is_some() || self.levels.is_some() {
                    return Err(format!("`{id}` is a choice: it takes `options`"));
                }
                let options = match (self.options, self.criteria) {
                    (Some(map), None) | (None, Some(Value::Object(map))) => map,
                    _ => {
                        return Err(format!(
                            "`{id}` is a choice and needs `options`: at least two names, each \
                             with a description"
                        ))
                    }
                };
                if options.len() < 2 {
                    return Err(format!("`{id}` is a choice and needs at least two options"));
                }
                if let Some(bad) = options.keys().find(|name| !is_option(name)) {
                    return Err(format!(
                        "`{bad}` is not an option name in `{id}`. Use lower-case letters, digits, \
                         `_` and `-`"
                    ));
                }
                Criteria::Choice {
                    options: options.into_iter().collect(),
                }
            }
            QuestionKind::Score => {
                if self.yes.is_some() || self.no.is_some() || self.options.is_some() {
                    return Err(format!("`{id}` is a score: it takes `levels`"));
                }
                let levels = match (self.levels, self.criteria) {
                    (Some(levels), None) | (None, Some(Value::Array(levels))) => levels,
                    _ => {
                        return Err(format!(
                            "`{id}` is a score and needs `levels`: at least two, lowest first"
                        ))
                    }
                };
                if levels.len() < 2 || !levels.iter().all(is_text) {
                    return Err(format!(
                        "`{id}` is a score and needs at least two non-empty levels"
                    ));
                }
                Criteria::Score { levels }
            }
        };

        Ok(Question {
            id,
            instructions: self.instructions,
            criteria,
        })
    }
}

/// Whether a choice option is named like one.
fn is_option(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Parses a model's `questions` array. Ids are required and unique.
pub fn parse_questions(drafts: Vec<QuestionDraft>) -> Result<Vec<Question>, String> {
    if drafts.is_empty() {
        return Err("ask at least one question".to_owned());
    }
    let mut questions: Vec<Question> = Vec::with_capacity(drafts.len());
    for mut draft in drafts {
        let Some(id) = draft.id.take().map(|id| id.trim().to_owned()) else {
            return Err(
                "every question needs an `id`, the key its answer comes back under".to_owned(),
            );
        };
        if questions.iter().any(|kept| kept.id == id) {
            return Err(format!("`{id}` is used twice; question ids are unique"));
        }
        questions.push(draft.check(id)?);
    }
    Ok(questions)
}

/// Checks a state: present, not empty, and under [`STATE_MAX_BYTES`].
pub fn check_state(state: &Value) -> Result<(), String> {
    let empty = match state {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Bool(_) | Value::Number(_) => {
            return Err("`state` is a string, an object or an array".to_owned())
        }
    };
    if empty {
        return Err("`state` is empty: give the structured facts the questions judge".to_owned());
    }
    let bytes = serde_json::to_vec(state).map_or(usize::MAX, |bytes| bytes.len());
    if bytes > STATE_MAX_BYTES {
        return Err(format!(
            "`state` is {bytes} bytes of JSON; the cap is {STATE_MAX_BYTES}. Keep the fields \
             the questions name, and leave the rest out"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

/// One answer, as the harness composes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    /// The probability that the statement holds. That probability *is* the
    /// uncertainty; there is no separate confidence.
    Noul {
        /// 0..1.
        noul: f64,
    },
    /// The selected option.
    Choice {
        /// The option's name.
        choice: String,
        /// Per option.
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
        /// 0..1.
        #[serde(default)]
        confidence: f64,
    },
    /// A position on the levels, possibly between two.
    Score {
        /// 0 is the first level.
        score: f64,
        /// 0..1.
        #[serde(default)]
        confidence: f64,
        /// Position → level, as TypeSafe echoes it, so `score` reads without
        /// the question beside it.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        legend: BTreeMap<String, Value>,
    },
}

/// What one request came back with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answers {
    /// The model that answered.
    #[serde(default)]
    pub model: String,
    /// By question id.
    pub answers: BTreeMap<String, Answer>,
}

impl Answers {
    /// A noul's probability.
    pub fn noul(&self, id: &str) -> Option<f64> {
        match self.answers.get(id)? {
            Answer::Noul { noul } => Some(*noul),
            _ => None,
        }
    }

    /// A choice and its confidence.
    pub fn choice(&self, id: &str) -> Option<(&str, f64)> {
        match self.answers.get(id)? {
            Answer::Choice {
                choice, confidence, ..
            } => Some((choice, *confidence)),
            _ => None,
        }
    }

    /// A score and its confidence.
    pub fn score(&self, id: &str) -> Option<(f64, f64)> {
        match self.answers.get(id)? {
            Answer::Score {
                score, confidence, ..
            } => Some((*score, *confidence)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Why no answer came back.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecisionError {
    /// No TypeSafe key in the credential store or the environment.
    #[error("no TypeSafe key is set. Add one in Settings, under Decision model")]
    NoKey,
    /// No HTTP client on this machine.
    #[error("Aegis has no HTTP client on this machine, so nothing could be sent")]
    NoClient,
    /// The origin in Settings is not usable.
    #[error("the decision base URL is not usable: {0}")]
    BadUrl(String),
    /// Nothing answered.
    #[error("TypeSafe could not be reached: {0}")]
    Transport(String),
    /// TypeSafe answered with an error.
    #[error("TypeSafe answered {status}: {message}")]
    Status {
        /// The HTTP status.
        status: u16,
        /// What it said, shortened.
        message: String,
    },
    /// The answer was not the documented shape.
    #[error("TypeSafe's answer could not be read: {0}")]
    Parse(String),
    /// The turn was stopped first.
    #[error("the request was cancelled")]
    Cancelled,
}

impl DecisionError {
    /// The envelope code for a tool that failed this way.
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::NoKey => ErrorCode::NoApiKey,
            Self::Parse(_) => ErrorCode::ProviderParse,
            Self::Cancelled => ErrorCode::Cancelled,
            Self::NoClient | Self::BadUrl(_) | Self::Transport(_) | Self::Status { .. } => {
                ErrorCode::ProviderHttp
            }
        }
    }
}

/// A configured decision client: a key, an endpoint, a model. Built per use
/// from Settings, so a change applies to the next request.
#[derive(Clone)]
pub struct DecisionClient {
    http: Client,
    key: ApiKey,
    endpoint: Url,
    model: String,
    annotate_approvals: bool,
}

impl std::fmt::Debug for DecisionClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionClient")
            .field("endpoint", &self.endpoint.as_str())
            .field("model", &self.model)
            .field("annotate_approvals", &self.annotate_approvals)
            .finish_non_exhaustive()
    }
}

/// The endpoint for an origin.
fn endpoint(origin: &str) -> Result<Url, DecisionError> {
    Url::parse(&format!("{}{ENDPOINT_PATH}", origin.trim_end_matches('/')))
        .map_err(|err| DecisionError::BadUrl(err.to_string()))
}

impl DecisionClient {
    /// A client for these settings. Fails closed: no key or no HTTP client is
    /// an error, never a request.
    pub fn new(
        http: Option<Client>,
        key: Option<ApiKey>,
        settings: &DecisionSettings,
    ) -> Result<Self, DecisionError> {
        let key = key.ok_or(DecisionError::NoKey)?;
        let http = http.ok_or(DecisionError::NoClient)?;
        Ok(Self {
            http,
            key,
            endpoint: endpoint(settings.resolved_base_url())?,
            model: settings.resolved_model().to_owned(),
            annotate_approvals: settings.annotate_approvals,
        })
    }

    /// The model requests name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Whether approval dialogs should be annotated.
    pub const fn annotates_approvals(&self) -> bool {
        self.annotate_approvals
    }

    /// The request body: `state`, `model`, and `questions` as a map by id.
    fn body(&self, state: &Value, questions: &[Question]) -> Value {
        let map: Map<String, Value> = questions
            .iter()
            .map(|question| (question.id.clone(), question.to_wire()))
            .collect();
        json!({ "state": state, "model": self.model, "questions": map })
    }

    /// Asks every question about `state` in one request, retrying a 429 or a
    /// 529. Stops early when `cancel` fires.
    pub async fn evaluate(
        &self,
        state: &Value,
        questions: &[Question],
        cancel: &CancellationToken,
    ) -> Result<Answers, DecisionError> {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(DecisionError::Cancelled),
            answered = self.evaluate_with_retries(state, questions, REQUEST_TIMEOUT) => answered,
        }
    }

    async fn evaluate_with_retries(
        &self,
        state: &Value,
        questions: &[Question],
        timeout: Duration,
    ) -> Result<Answers, DecisionError> {
        let body = self.body(state, questions);
        let mut attempt = 0;
        loop {
            match self.send(&body, timeout).await {
                Err(DecisionError::Status { status, .. })
                    if (status == 429 || status == OVERLOADED) && attempt < BACKOFF.len() =>
                {
                    tracing::info!(status, attempt, "TypeSafe asked to back off");
                    tokio::time::sleep(BACKOFF[attempt]).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    /// One request.
    async fn send(&self, body: &Value, timeout: Duration) -> Result<Answers, DecisionError> {
        let response = self
            .http
            .post(self.endpoint.clone())
            .timeout(timeout)
            .header(AUTHORIZATION, format!("Bearer {}", self.key.expose()))
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .map_err(|err| {
                tracing::warn!(%err, "a decision request did not reach TypeSafe");
                DecisionError::Transport(if err.is_timeout() {
                    "it did not answer in time".to_owned()
                } else if err.is_connect() {
                    "the connection failed".to_owned()
                } else {
                    "the request failed".to_owned()
                })
            })?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            let message: String = text.trim().chars().take(ERROR_BODY_CHARS).collect();
            tracing::warn!(
                status = status.as_u16(),
                "TypeSafe refused a decision request"
            );
            return Err(DecisionError::Status {
                status: status.as_u16(),
                message: status_reason(status, &message),
            });
        }

        response
            .json::<Answers>()
            .await
            .map_err(|err| DecisionError::Parse(err.to_string()))
    }

    /// One cheap noul against a fixed state, reported like a chat probe.
    pub async fn probe(&self) -> ProviderProbe {
        let question = Question {
            id: "probe".to_owned(),
            instructions: json!("Does `greeting` say hello?"),
            criteria: Criteria::Noul {
                yes: None,
                no: None,
            },
        };
        let started = Instant::now();
        let outcome = self
            .send(
                &self.body(&json!({ "greeting": "hello" }), &[question]),
                PROBE_TIMEOUT,
            )
            .await;
        let latency_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));

        match outcome {
            Ok(answers) => ProviderProbe {
                ok: answers.noul("probe").is_some(),
                status: Some(200),
                latency_ms,
                message: match answers.noul("probe") {
                    Some(noul) => format!(
                        "TypeSafe answered as `{}` (probe noul {noul:.2}).",
                        if answers.model.is_empty() {
                            &self.model
                        } else {
                            &answers.model
                        }
                    ),
                    None => "TypeSafe answered, without the probe's answer.".to_owned(),
                },
            },
            Err(DecisionError::Status { status, message }) => ProviderProbe {
                ok: false,
                status: Some(status),
                latency_ms,
                message,
            },
            Err(err) => ProviderProbe {
                ok: false,
                status: None,
                latency_ms: None,
                message: format!("{}.", capitalize(&err.to_string())),
            },
        }
    }
}

/// The probe's answer when no client could be built.
pub fn unusable_probe(err: &DecisionError) -> ProviderProbe {
    ProviderProbe {
        ok: false,
        status: None,
        latency_ms: None,
        message: format!("{}.", capitalize(&err.to_string())),
    }
}

/// A status, told apart the way PLAN 7.18 *Probe* asks.
fn status_reason(status: StatusCode, body: &str) -> String {
    let detail = if body.is_empty() {
        String::new()
    } else {
        format!(" ({body})")
    };
    match status.as_u16() {
        401 | 403 => format!("TypeSafe rejected the key ({status}).{detail}"),
        422 => format!("TypeSafe refused the request as invalid ({status}).{detail}"),
        429 => format!("The key works, but TypeSafe is rate limiting it ({status})."),
        OVERLOADED => "TypeSafe is overloaded (529); try again shortly.".to_owned(),
        404 => format!("There is no System One endpoint at this origin ({status})."),
        _ => format!("TypeSafe answered {status}.{detail}"),
    }
}

/// Upper-cases the first letter.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(value: Value) -> QuestionDraft {
        serde_json::from_value(value).expect("a draft")
    }

    #[test]
    fn a_choice_without_options_is_refused() {
        let err = parse_questions(vec![draft(json!({
            "id": "kind", "type": "choice", "instructions": "Which?"
        }))])
        .expect_err("refused");
        assert!(err.contains("options"), "{err}");
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let one = json!({ "id": "a", "type": "noul", "instructions": "Is it?" });
        let err = parse_questions(vec![draft(one.clone()), draft(one)]).expect_err("refused");
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn a_score_needs_two_levels_and_ids_are_shaped() {
        assert!(parse_questions(vec![draft(json!({
            "id": "undo", "type": "score", "instructions": "How hard?", "levels": ["easy"]
        }))])
        .is_err());
        assert!(parse_questions(vec![draft(json!({
            "id": "Bad Id", "type": "noul", "instructions": "Is it?"
        }))])
        .is_err());
    }

    #[test]
    fn an_empty_state_is_refused_and_a_large_one_too() {
        assert!(check_state(&json!({})).is_err());
        assert!(check_state(&json!("  ")).is_err());
        assert!(check_state(&json!(3)).is_err());
        assert!(check_state(&json!({ "a": "x".repeat(STATE_MAX_BYTES) })).is_err());
        assert!(check_state(&json!({ "ticket": { "text": "hi" } })).is_ok());
    }

    #[test]
    fn questions_go_out_as_a_map_with_typesafe_criteria() {
        let settings = DecisionSettings::default();
        let key = ApiKey::new("ts-key");
        let client =
            DecisionClient::new(Client::builder().build().ok(), key, &settings).expect("a client");
        let questions = parse_questions(vec![
            draft(json!({ "id": "risky", "type": "noul", "instructions": "Risky?", "yes": "it deletes" })),
            draft(json!({ "id": "kind", "type": "choice", "instructions": "Which?",
                          "options": { "read": "reads", "write": { "what": "writes" } } })),
            draft(json!({ "id": "undo", "type": "score", "instructions": "Undo?",
                          "criteria": ["easy", "hard"] })),
        ])
        .expect("parsed");

        let body = client.body(&json!({ "tool": "fs_write" }), &questions);
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["state"]["tool"], "fs_write", "structure is kept");
        assert_eq!(body["questions"]["risky"]["criteria"]["true"], "it deletes");
        assert_eq!(
            body["questions"]["kind"]["criteria"]["write"]["what"],
            "writes"
        );
        assert_eq!(body["questions"]["undo"]["criteria"][1], "hard");
        assert!(body["questions"]["risky"].get("id").is_none());
        assert_eq!(
            client.endpoint.as_str(),
            "https://api.typesafe.ai/v1/systemone"
        );
    }

    #[test]
    fn no_key_fails_closed() {
        let err = DecisionClient::new(
            Client::builder().build().ok(),
            None,
            &DecisionSettings::default(),
        )
        .expect_err("no key");
        assert_eq!(err, DecisionError::NoKey);
        assert_eq!(err.code(), ErrorCode::NoApiKey);
    }

    #[test]
    fn answers_parse_per_type() {
        let answers: Answers = serde_json::from_value(json!({
            "model": "jev-1",
            "answers": {
                "a": { "type": "noul", "noul": 0.9 },
                "b": { "type": "choice", "choice": "x", "probabilities": { "x": 0.8 }, "confidence": 0.7 },
                "c": { "type": "score", "score": 1.4, "legend": { "0": "l" }, "probabilities": {}, "confidence": 0.5 }
            },
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }))
        .expect("parsed");
        assert_eq!(answers.noul("a"), Some(0.9));
        assert_eq!(answers.choice("b"), Some(("x", 0.7)));
        assert_eq!(answers.score("c"), Some((1.4, 0.5)));
        // The legend is kept, so a result the model reads names its levels.
        let rendered = serde_json::to_value(&answers).expect("serializes");
        assert_eq!(rendered["answers"]["c"]["legend"]["0"], "l");
        assert_eq!(answers.noul("b"), None);
    }
}
