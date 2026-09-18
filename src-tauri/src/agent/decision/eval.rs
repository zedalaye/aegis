//! Project evals: `.aegis/evals/<name>/eval.yml` (PLAN 7.18, *Project evals*).
//!
//! An eval is one System One request plus a bounded composition, written in
//! a file a person signed. Only `eval.yml` runs; `PROPOSAL.yml` beside it is a
//! draft, applied by an `fs_write` that copies it byte for byte (the § 7.13
//! analogue). The composition is a closed vocabulary interpreted here: it
//! reads answers and produces route labels and escalations — data, never an
//! action.
//!
//! ```yaml
//! name: inbox.classify
//! when: Classify the newest inbound mail
//! inputs:
//!   mail: inbox/latest.md
//! confidence: 0.6
//! questions:
//!   is_invoice: { type: noul, instructions: "Does `mail` contain an invoice?" }
//!   topic:
//!     type: choice
//!     instructions: What is `mail` about?
//!     options: { billing: money owed, support: a problem to fix }
//! weights:
//!   urgency: { is_invoice: 0.4 }
//! compose:
//!   - when: noul is_invoice >= 0.8
//!     route: billing
//!   - when: [choice topic is support, weight urgency >= 0.3]
//!     escalate: needs_you
//!     say: A support mail that looks urgent.
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use super::{Answers, Question, QuestionDraft, QuestionKind};
use crate::skills::{self, ProposalState};
use crate::workspace;

/// The evals directory under the cabinet.
pub const EVALS_DIR: &str = "evals";

/// The runnable file.
pub const EVAL_FILE: &str = "eval.yml";

/// The draft beside it.
pub const PROPOSAL_FILE: &str = "PROPOSAL.yml";

/// Longest `eval.yml` read.
const FILE_MAX_BYTES: u64 = 64 * 1024;

/// Most inputs one eval reads.
const INPUTS_MAX: usize = 16;

/// Most composition rules.
const RULES_MAX: usize = 64;

/// Longest escalation sentence.
const SAY_MAX_CHARS: usize = 240;

/// Longest `when` line.
const WHEN_MAX_CHARS: usize = 160;

/// Default confidence floor for choices and scores.
const DEFAULT_CONFIDENCE: f64 = 0.6;

/// The evals directory of a workspace.
pub fn evals_dir(root: &Path) -> PathBuf {
    root.join(workspace::CABINET_DIR).join(EVALS_DIR)
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

/// The file as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalFile {
    name: String,
    #[serde(default)]
    when: String,
    #[serde(default)]
    inputs: BTreeMap<String, String>,
    #[serde(default)]
    confidence: Option<f64>,
    questions: BTreeMap<String, QuestionDraft>,
    #[serde(default)]
    weights: BTreeMap<String, BTreeMap<String, f64>>,
    #[serde(default)]
    compose: Vec<RuleFile>,
}

/// One rule as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    when: OneOrMany,
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    escalate: Option<String>,
    #[serde(default)]
    say: Option<String>,
}

/// A condition, or several that must all hold.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

/// A comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    /// `>=`
    Ge,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `<`
    Lt,
}

impl Cmp {
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            ">=" => Self::Ge,
            ">" => Self::Gt,
            "<=" => Self::Le,
            "<" => Self::Lt,
            _ => return None,
        })
    }

    fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::Ge => value >= threshold,
            Self::Gt => value > threshold,
            Self::Le => value <= threshold,
            Self::Lt => value < threshold,
        }
    }
}

/// One condition of the closed vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    /// `noul <id> <cmp> <t>` — the probability itself.
    Noul(String, Cmp, f64),
    /// `choice <id> is <option>` — only above the confidence floor.
    Choice(String, String),
    /// `score <id> <cmp> <t>` — only above the confidence floor.
    Score(String, Cmp, f64),
    /// `weight <name> <cmp> <t>` — a named sum of nouls.
    Weight(String, Cmp, f64),
}

/// What a rule produces when its conditions hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A label for the caller. Not an action.
    Route(String),
    /// A human is needed.
    Escalate {
        /// `needs_you` or `blocked`.
        status: String,
        /// One sentence.
        say: String,
    },
}

/// One composition rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// All must hold.
    pub when: Vec<Condition>,
    /// What follows.
    pub then: Outcome,
}

/// A signed eval, parsed and checked.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalDoc {
    /// Its name — the directory's.
    pub name: String,
    /// The catalog line.
    pub when: String,
    /// State key → workspace-relative path.
    pub inputs: BTreeMap<String, String>,
    /// The floor for choices and scores.
    pub confidence: f64,
    /// The questions, by id.
    pub questions: Vec<Question>,
    /// Named sums of nouls.
    pub weights: BTreeMap<String, BTreeMap<String, f64>>,
    /// The rules, in order.
    pub rules: Vec<Rule>,
}

fn is_fraction(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

/// Parses one condition against the questions and weights it may name.
fn condition(
    text: &str,
    questions: &[Question],
    weights: &BTreeMap<String, BTreeMap<String, f64>>,
) -> Result<Condition, String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let kind_of = |id: &str| questions.iter().find(|q| q.id == id).map(Question::kind);
    let number = |word: &str| {
        word.parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("`{word}` in `{text}` is not a number"))
    };
    let expect = |id: &str, want: QuestionKind| match kind_of(id) {
        Some(kind) if kind == want => Ok(()),
        Some(kind) => Err(format!(
            "`{text}` reads `{id}` as a {}, but it is a {}",
            want.as_str(),
            kind.as_str()
        )),
        None => Err(format!(
            "`{text}` names `{id}`, which is not a question here"
        )),
    };

    match words.as_slice() {
        ["noul", id, cmp, t] => {
            expect(id, QuestionKind::Noul)?;
            let t = number(t)?;
            if !is_fraction(t) {
                return Err(format!("`{text}`: a noul threshold is between 0 and 1"));
            }
            let cmp = Cmp::parse(cmp).ok_or_else(|| format!("`{cmp}` is not a comparison"))?;
            Ok(Condition::Noul((*id).to_owned(), cmp, t))
        }
        ["choice", id, "is", option] => {
            expect(id, QuestionKind::Choice)?;
            let known = questions
                .iter()
                .find(|q| q.id == *id)
                .is_some_and(|q| q.options().contains(option));
            if !known {
                return Err(format!("`{text}`: `{option}` is not an option of `{id}`"));
            }
            Ok(Condition::Choice((*id).to_owned(), (*option).to_owned()))
        }
        ["score", id, cmp, t] => {
            expect(id, QuestionKind::Score)?;
            let cmp = Cmp::parse(cmp).ok_or_else(|| format!("`{cmp}` is not a comparison"))?;
            Ok(Condition::Score((*id).to_owned(), cmp, number(t)?))
        }
        ["weight", name, cmp, t] => {
            if !weights.contains_key(*name) {
                return Err(format!(
                    "`{text}` names `{name}`, which is not under `weights`"
                ));
            }
            let cmp = Cmp::parse(cmp).ok_or_else(|| format!("`{cmp}` is not a comparison"))?;
            Ok(Condition::Weight((*name).to_owned(), cmp, number(t)?))
        }
        _ => Err(format!(
            "`{text}` is not a condition. The forms are `noul <id> >= <t>`, `choice <id> is \
             <option>`, `score <id> >= <t>` and `weight <name> >= <t>`"
        )),
    }
}

/// Whether an input path is plain and relative: no root, no `..`.
fn is_plain_relative(path: &str) -> bool {
    let path = Path::new(path.trim());
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Parses an eval's text. `dir_name` is the directory it lives in, which the
/// `name` must match.
pub fn parse(dir_name: &str, text: &str) -> Result<EvalDoc, String> {
    let file: EvalFile =
        serde_yaml_ng::from_str(text).map_err(|err| format!("it is not a valid eval: {err}"))?;

    if file.name.trim() != dir_name {
        return Err(format!(
            "`name` is `{}`, but the directory is `{dir_name}`; they must match",
            file.name.trim()
        ));
    }
    let when = file.when.trim().to_owned();
    if when.contains('\n') || when.chars().count() > WHEN_MAX_CHARS {
        return Err(format!(
            "`when` is one line of at most {WHEN_MAX_CHARS} characters"
        ));
    }
    let confidence = file.confidence.unwrap_or(DEFAULT_CONFIDENCE);
    if !is_fraction(confidence) {
        return Err("`confidence` is between 0 and 1".to_owned());
    }

    if file.inputs.len() > INPUTS_MAX {
        return Err(format!("at most {INPUTS_MAX} inputs"));
    }
    for (key, path) in &file.inputs {
        if !super::is_id(key) {
            return Err(format!(
                "`{key}` is not an input key (lower-case, digits, `_`)"
            ));
        }
        if !is_plain_relative(path) {
            return Err(format!(
                "input `{key}` is `{path}`; inputs are paths relative to the workspace, without `..`"
            ));
        }
    }

    if file.questions.is_empty() {
        return Err("an eval asks at least one question".to_owned());
    }
    let mut questions = Vec::with_capacity(file.questions.len());
    for (id, draft) in file.questions {
        if draft.id.as_deref().is_some_and(|inner| inner != id) {
            return Err(format!("question `{id}` carries a different `id`; drop it"));
        }
        questions.push(draft.check(id)?);
    }

    for (name, terms) in &file.weights {
        if !super::is_id(name) || terms.is_empty() {
            return Err(format!(
                "weight `{name}` needs a plain name and at least one term"
            ));
        }
        for (id, coefficient) in terms {
            match questions.iter().find(|q| &q.id == id) {
                Some(q) if q.kind() == QuestionKind::Noul => {}
                Some(_) => {
                    return Err(format!("weight `{name}` names `{id}`, which is not a noul"))
                }
                None => {
                    return Err(format!(
                        "weight `{name}` names `{id}`, which is not a question"
                    ))
                }
            }
            if !coefficient.is_finite() {
                return Err(format!(
                    "weight `{name}`: `{id}`'s coefficient is not a number"
                ));
            }
        }
    }

    if file.compose.len() > RULES_MAX {
        return Err(format!("at most {RULES_MAX} compose rules"));
    }
    let mut rules = Vec::with_capacity(file.compose.len());
    for (at, rule) in file.compose.into_iter().enumerate() {
        let texts = match rule.when {
            OneOrMany::One(text) => vec![text],
            OneOrMany::Many(texts) => texts,
        };
        if texts.is_empty() {
            return Err(format!("rule {} has no condition", at + 1));
        }
        let when = texts
            .iter()
            .map(|text| condition(text, &questions, &file.weights))
            .collect::<Result<Vec<_>, _>>()?;

        let then = match (rule.route, rule.escalate) {
            (Some(label), None) => {
                if rule.say.is_some() {
                    return Err(format!("rule {}: `say` goes with `escalate`", at + 1));
                }
                let label = label.trim().to_owned();
                if !skills::is_name(&label) {
                    return Err(format!(
                        "rule {}: `{label}` is not a route label (lower-case letters, digits, \
                         `.`, `-`, `_`)",
                        at + 1
                    ));
                }
                Outcome::Route(label)
            }
            (None, Some(status)) => {
                let status = status.trim().to_owned();
                if status != "needs_you" && status != "blocked" {
                    return Err(format!(
                        "rule {}: escalate to `needs_you` or `blocked`, not `{status}`",
                        at + 1
                    ));
                }
                let say = rule.say.unwrap_or_default().trim().to_owned();
                if say.is_empty() || say.contains('\n') || say.chars().count() > SAY_MAX_CHARS {
                    return Err(format!(
                        "rule {}: an escalation says one sentence (`say`, at most \
                         {SAY_MAX_CHARS} characters)",
                        at + 1
                    ));
                }
                Outcome::Escalate { status, say }
            }
            _ => {
                return Err(format!(
                    "rule {} needs exactly one of `route` and `escalate`",
                    at + 1
                ))
            }
        };
        rules.push(Rule { when, then });
    }

    Ok(EvalDoc {
        name: dir_name.to_owned(),
        when,
        inputs: file.inputs,
        confidence,
        questions,
        weights: file.weights,
        rules,
    })
}

/// Reads a file, capped.
fn read_capped(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|_| "it cannot be read".to_owned())?;
    if meta.len() > FILE_MAX_BYTES {
        return Err(format!("it is over {} KB", FILE_MAX_BYTES / 1024));
    }
    fs::read_to_string(path).map_err(|_| "it is not UTF-8 text".to_owned())
}

/// What loading a named eval found.
#[derive(Debug)]
pub enum Loaded {
    /// A signed eval.
    Ready(Box<EvalDoc>),
    /// No `eval.yml`; `proposed` says whether a `PROPOSAL.yml` waits.
    Missing {
        /// A draft is there, not applied.
        proposed: bool,
    },
    /// There, and broken.
    Broken(String),
}

/// Loads `.aegis/evals/<name>/eval.yml`, and only that.
pub fn load(root: &Path, name: &str) -> Loaded {
    if !skills::is_name(name) {
        return Loaded::Broken(format!("`{name}` is not an eval name"));
    }
    let dir = evals_dir(root).join(name);
    let file = dir.join(EVAL_FILE);
    if !file.is_file() {
        return Loaded::Missing {
            proposed: dir.join(PROPOSAL_FILE).is_file(),
        };
    }
    match read_capped(&file).and_then(|text| parse(name, &text)) {
        Ok(doc) => Loaded::Ready(Box::new(doc)),
        Err(problem) => Loaded::Broken(problem),
    }
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// One signed eval, as Settings lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct EvalEntry {
    /// Its name.
    pub name: String,
    /// The catalog line. Empty when it will not parse.
    pub when: String,
    /// Its input keys and paths, `key: path`.
    pub inputs: Vec<String>,
    /// Its question ids.
    pub questions: Vec<String>,
    /// The file.
    pub path: String,
    /// Why it would not run.
    pub problem: Option<String>,
}

/// One `PROPOSAL.yml`, listed apart so it is never offered as runnable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct EvalProposal {
    /// The name it would run under.
    pub name: String,
    /// The catalog line, when it parses.
    pub when: String,
    /// Its question ids.
    pub questions: Vec<String>,
    /// The proposal file.
    pub path: String,
    /// The `eval.yml` applying it would write.
    pub target: String,
    /// Where it stands.
    pub state: ProposalState,
    /// Why it would not apply.
    pub problem: Option<String>,
}

/// Directory names under `evals/` holding `file`.
fn named_dirs(root: &Path, file: &str) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(evals_dir(root)) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path().join(file);
            (skills::is_name(&name) && path.is_file()).then_some((name, path))
        })
        .collect();
    found.sort();
    found
}

/// Every signed eval of a workspace. Never fails.
pub fn list(root: &Path) -> Vec<EvalEntry> {
    named_dirs(root, EVAL_FILE)
        .into_iter()
        .map(|(name, path)| {
            let parsed = read_capped(&path).and_then(|text| parse(&name, &text));
            let mut entry = EvalEntry {
                name,
                when: String::new(),
                inputs: Vec::new(),
                questions: Vec::new(),
                path: path.display().to_string(),
                problem: None,
            };
            match parsed {
                Ok(doc) => {
                    entry.when = doc.when;
                    entry.inputs = doc
                        .inputs
                        .iter()
                        .map(|(key, path)| format!("{key}: {path}"))
                        .collect();
                    entry.questions = doc.questions.into_iter().map(|q| q.id).collect();
                }
                Err(problem) => entry.problem = Some(problem),
            }
            entry
        })
        .collect()
}

/// Every `PROPOSAL.yml` of a workspace. Never fails.
pub fn proposals(root: &Path) -> Vec<EvalProposal> {
    named_dirs(root, PROPOSAL_FILE)
        .into_iter()
        .map(|(name, path)| {
            let target = path.with_file_name(EVAL_FILE);
            let text = fs::read(&path).ok();
            let state = match (fs::read(&target), &text) {
                (Err(_), _) => ProposalState::Pending,
                (Ok(live), Some(proposed)) if &live == proposed => ProposalState::Applied,
                (Ok(_), _) => ProposalState::Occupied,
            };
            let mut proposal = EvalProposal {
                name: name.clone(),
                when: String::new(),
                questions: Vec::new(),
                path: path.display().to_string(),
                target: target.display().to_string(),
                state,
                problem: None,
            };
            match read_capped(&path).and_then(|text| parse(&name, &text)) {
                Ok(doc) => {
                    proposal.when = doc.when;
                    proposal.questions = doc.questions.into_iter().map(|q| q.id).collect();
                }
                Err(problem) => proposal.problem = Some(problem),
            }
            proposal
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Applying a proposal, and writes that are not one
// ---------------------------------------------------------------------------

/// What a write under `.aegis/evals/<name>/eval.yml` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalWrite {
    /// A byte-for-byte copy of the `PROPOSAL.yml` beside it, which may be put
    /// to a person.
    Apply(String),
    /// A copy that is refused, with why.
    Refused(String),
    /// Any other content: a direct write of a runnable eval.
    Direct(String),
}

/// Recognizes a write of an `eval.yml`. `relative` is the target under the
/// workspace root. `None` for every other path.
pub fn write_of(root: &Path, relative: &Path, content: &str) -> Option<EvalWrite> {
    let mut parts = relative.components().filter_map(|part| match part {
        Component::Normal(name) => name.to_str(),
        _ => None,
    });
    let (Some(cabinet), Some(evals), Some(name), Some(file), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    if !cabinet.eq_ignore_ascii_case(workspace::CABINET_DIR)
        || !evals.eq_ignore_ascii_case(EVALS_DIR)
        || !file.eq_ignore_ascii_case(EVAL_FILE)
    {
        return None;
    }
    if !skills::is_name(name) {
        return Some(EvalWrite::Refused(format!("`{name}` is not an eval name")));
    }

    let dir = evals_dir(root).join(name);
    let proposed = fs::read(dir.join(PROPOSAL_FILE)).ok();
    if proposed.as_deref() != Some(content.as_bytes()) {
        return Some(EvalWrite::Direct(name.to_owned()));
    }
    if let Err(problem) = parse(name, content) {
        return Some(EvalWrite::Refused(format!(
            "`{name}`'s proposal does not parse, and a proposal that does not parse is never \
             applied: {problem}. Fix `{PROPOSAL_FILE}` first"
        )));
    }
    if dir.join(EVAL_FILE).exists() {
        return Some(EvalWrite::Refused(format!(
            "there is already an `{EVAL_FILE}` for `{name}`, and applying a proposal never \
             replaces one. Propose it under a new name, or leave the edit to a person"
        )));
    }
    Some(EvalWrite::Apply(name.to_owned()))
}

// ---------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------

/// An escalation, as returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Escalation {
    /// `needs_you` or `blocked`.
    pub status: String,
    /// One sentence.
    pub say: String,
}

/// What an eval returns: routes, escalations, and the answers they rest on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Composed {
    /// The eval.
    pub eval: String,
    /// Route labels, in rule order, each once. Recommendations, not actions.
    pub routes: Vec<String>,
    /// Escalations, in rule order.
    pub escalations: Vec<Escalation>,
    /// Choices above the floor.
    pub choices: BTreeMap<String, String>,
    /// Choices and scores under the floor.
    pub uncertain: Vec<String>,
    /// The named sums.
    pub weights: BTreeMap<String, f64>,
    /// Every answer as it came back.
    pub answers: Answers,
}

/// Applies an eval's rules to its answers. A question with no answer makes
/// every condition on it false.
pub fn compose(doc: &EvalDoc, answers: Answers) -> Composed {
    let floor = doc.confidence;
    let weights: BTreeMap<String, f64> = doc
        .weights
        .iter()
        .map(|(name, terms)| {
            let sum = terms
                .iter()
                .map(|(id, coefficient)| coefficient * answers.noul(id).unwrap_or(0.0))
                .sum();
            (name.clone(), sum)
        })
        .collect();

    let mut choices = BTreeMap::new();
    let mut uncertain = Vec::new();
    for question in &doc.questions {
        match question.kind() {
            QuestionKind::Choice => match answers.choice(&question.id) {
                Some((choice, confidence)) if confidence >= floor => {
                    choices.insert(question.id.clone(), choice.to_owned());
                }
                _ => uncertain.push(question.id.clone()),
            },
            QuestionKind::Score => {
                if !answers
                    .score(&question.id)
                    .is_some_and(|(_, confidence)| confidence >= floor)
                {
                    uncertain.push(question.id.clone());
                }
            }
            QuestionKind::Noul => {}
        }
    }

    let holds = |condition: &Condition| match condition {
        Condition::Noul(id, cmp, t) => answers.noul(id).is_some_and(|p| cmp.holds(p, *t)),
        Condition::Choice(id, option) => choices.get(id) == Some(option),
        Condition::Score(id, cmp, t) => answers
            .score(id)
            .is_some_and(|(score, confidence)| confidence >= floor && cmp.holds(score, *t)),
        Condition::Weight(name, cmp, t) => weights.get(name).is_some_and(|w| cmp.holds(*w, *t)),
    };

    let mut routes: Vec<String> = Vec::new();
    let mut escalations = Vec::new();
    for rule in &doc.rules {
        if !rule.when.iter().all(holds) {
            continue;
        }
        match &rule.then {
            Outcome::Route(label) => {
                if !routes.contains(label) {
                    routes.push(label.clone());
                }
            }
            Outcome::Escalate { status, say } => escalations.push(Escalation {
                status: status.clone(),
                say: say.clone(),
            }),
        }
    }

    Composed {
        eval: doc.name.clone(),
        routes,
        escalations,
        choices,
        uncertain,
        weights,
        answers,
    }
}

/// The state object: each input key holding its file's text.
pub fn state_of(texts: &[(String, String)]) -> Value {
    Value::Object(
        texts
            .iter()
            .map(|(key, text)| (key.clone(), Value::String(text.clone())))
            .collect::<Map<String, Value>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agent::decision::Answer;
    use tempfile::TempDir;

    pub(crate) const SAMPLE: &str = r#"name: inbox.classify
when: Classify the newest inbound mail
inputs:
  mail: inbox/latest.md
questions:
  is_invoice:
    type: noul
    instructions: Does `mail` contain an invoice?
  topic:
    type: choice
    instructions: What is `mail` about?
    options:
      billing: money owed
      support: a problem to fix
weights:
  urgency: { is_invoice: 0.5 }
compose:
  - when: noul is_invoice >= 0.8
    route: billing
  - when: [choice topic is support, weight urgency >= 0.3]
    escalate: needs_you
    say: A support mail that looks urgent.
"#;

    fn answers(invoice: f64, topic: &str, confidence: f64) -> Answers {
        let mut map = BTreeMap::new();
        map.insert("is_invoice".to_owned(), Answer::Noul { noul: invoice });
        map.insert(
            "topic".to_owned(),
            Answer::Choice {
                choice: topic.to_owned(),
                probabilities: BTreeMap::new(),
                confidence,
            },
        );
        Answers {
            model: "jev".to_owned(),
            answers: map,
        }
    }

    #[test]
    fn a_sample_eval_parses_and_routes() {
        let doc = parse("inbox.classify", SAMPLE).expect("parses");
        assert_eq!(doc.inputs["mail"], "inbox/latest.md");
        assert_eq!(doc.rules.len(), 2);

        let routed = compose(&doc, answers(0.9, "billing", 0.9));
        assert_eq!(routed.routes, ["billing"]);
        assert!(routed.escalations.is_empty());

        let escalated = compose(&doc, answers(0.7, "support", 0.9));
        assert!(escalated.routes.is_empty());
        assert_eq!(escalated.escalations[0].status, "needs_you");
    }

    #[test]
    fn a_low_confidence_choice_is_uncertain_and_routes_nothing() {
        let doc = parse("inbox.classify", SAMPLE).expect("parses");
        let composed = compose(&doc, answers(0.7, "support", 0.2));
        assert_eq!(composed.uncertain, ["topic"]);
        assert!(composed.escalations.is_empty());
        assert!(composed.choices.is_empty());
    }

    #[test]
    fn the_vocabulary_is_closed() {
        let refused = [
            SAMPLE.replace("route: billing", "route: billing\n    escalate: blocked"),
            SAMPLE.replace("noul is_invoice >= 0.8", "shell rm -rf /"),
            SAMPLE.replace("noul is_invoice >= 0.8", "noul topic >= 0.8"),
            SAMPLE.replace("choice topic is support", "choice topic is refunds"),
            SAMPLE.replace("inbox/latest.md", "../secrets.md"),
            SAMPLE.replace("name: inbox.classify", "name: other"),
            SAMPLE.replace("route: billing", "route: billing\n    run: git push"),
            SAMPLE.replace("say: A support mail that looks urgent.", "say: \"\""),
        ];
        for text in refused {
            assert!(parse("inbox.classify", &text).is_err(), "accepted:\n{text}");
        }
    }

    fn propose(root: &Path, name: &str, file: &str, text: &str) {
        let dir = evals_dir(root).join(name);
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join(file), text).expect("write");
    }

    #[test]
    fn only_a_byte_for_byte_copy_is_an_apply() {
        let root = TempDir::new().expect("temp");
        propose(root.path(), "inbox.classify", PROPOSAL_FILE, SAMPLE);
        let target = Path::new(".aegis/evals/inbox.classify/eval.yml");

        assert_eq!(
            write_of(root.path(), target, SAMPLE),
            Some(EvalWrite::Apply("inbox.classify".to_owned()))
        );
        assert_eq!(
            write_of(root.path(), target, "name: inbox.classify\n"),
            Some(EvalWrite::Direct("inbox.classify".to_owned()))
        );
        assert_eq!(
            write_of(root.path(), Path::new("notes/eval.yml"), SAMPLE),
            None
        );

        propose(root.path(), "inbox.classify", EVAL_FILE, "old");
        assert!(matches!(
            write_of(root.path(), target, SAMPLE),
            Some(EvalWrite::Refused(_))
        ));
    }

    #[test]
    fn a_proposal_is_not_loaded_and_is_listed_apart() {
        let root = TempDir::new().expect("temp");
        propose(root.path(), "inbox.classify", PROPOSAL_FILE, SAMPLE);

        assert!(matches!(
            load(root.path(), "inbox.classify"),
            Loaded::Missing { proposed: true }
        ));
        assert!(list(root.path()).is_empty());
        let proposals = proposals(root.path());
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].state, ProposalState::Pending);

        propose(root.path(), "inbox.classify", EVAL_FILE, SAMPLE);
        assert!(matches!(
            load(root.path(), "inbox.classify"),
            Loaded::Ready(_)
        ));
        assert_eq!(list(root.path())[0].questions, ["is_invoice", "topic"]);
        assert_eq!(proposals_state(root.path()), ProposalState::Applied);
    }

    fn proposals_state(root: &Path) -> ProposalState {
        proposals(root)[0].state
    }
}
