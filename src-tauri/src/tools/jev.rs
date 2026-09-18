//! `jev_eval` and `jev_ask`: the decision model as tools (PLAN 7.18).
//!
//! Both run only after approval. A missing TypeSafe key is an envelope here,
//! never a policy decision. What comes back is data — routes, escalations,
//! probabilities — and nothing in this file acts on it.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{moved, Produced};
use crate::agent::decision::eval::{self, EvalDoc};
use crate::agent::decision::{self, DecisionClient, DecisionError, Question, STATE_MAX_BYTES};
use crate::error::ErrorCode;
use crate::policy::tool;

/// Schema of `jev_eval`.
pub fn eval_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "The eval's name: a directory under `.aegis/evals/` holding a signed `eval.yml`."
            },
            "inputs": {
                "type": "object",
                "additionalProperties": { "type": "string" },
                "description": "Optional. Workspace paths replacing the file's declared inputs, by input key. Only declared keys."
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

/// Schema of `jev_ask`.
pub fn ask_schema() -> Value {
    let text = json!({
        "description": "A string, or an object (`what`, `not_for`, `examples`) when a boundary needs contrast.",
        "anyOf": [ { "type": "string" }, { "type": "object" }, { "type": "array" } ]
    });
    json!({
        "type": "object",
        "properties": {
            "state": {
                "description": "The facts judged: a JSON object with named fields (preferred), or one passage as a string. At most 64 KB of JSON. Never a transcript.",
                "anyOf": [ { "type": "object" }, { "type": "array" }, { "type": "string" } ]
            },
            "questions": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "The key the answer comes back under: lower-case, digits, `_`." },
                        "type": { "type": "string", "enum": ["noul", "choice", "score"] },
                        "instructions": text,
                        "yes": text,
                        "no": text,
                        "options": { "type": "object", "description": "choice: option name → description; at least two." },
                        "levels": { "type": "array", "description": "score: at least two levels, lowest first." }
                    },
                    "required": ["id", "type", "instructions"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["state", "questions"],
        "additionalProperties": false
    })
}

/// The envelope for a call made with no client. The runtime builds none
/// without a key, or without an HTTP client (it logs which).
fn no_client(name: &str) -> Produced {
    let err = DecisionError::NoKey;
    Produced::failed(name, err.code(), format!("{err}"))
}

/// The envelope for a finished request.
fn answered(name: &str, summary: String, body: &impl serde::Serialize) -> Produced {
    let content = serde_json::to_string_pretty(body).unwrap_or_default();
    let bytes = content.len() as u64;
    Produced::ok(name, summary, content, bytes, false, json!({}))
}

/// Runs a signed eval: reads its inputs, asks, composes.
pub(crate) async fn run_eval(
    configured: Option<&DecisionClient>,
    doc: &EvalDoc,
    inputs: &[(String, PathBuf)],
    cancel: &CancellationToken,
) -> Produced {
    let name = tool::JEV_EVAL;
    let Some(client) = configured else {
        return no_client(name);
    };

    let mut texts = Vec::with_capacity(inputs.len());
    let mut total = 0usize;
    for (key, path) in inputs {
        if let Some(refused) = moved(name, path) {
            return refused;
        }
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                return Produced::failed(
                    name,
                    ErrorCode::ToolFailed,
                    format!("input `{key}` could not be read as text: {err}"),
                )
            }
        };
        total += text.len();
        if total > STATE_MAX_BYTES {
            return Produced::failed(
                name,
                ErrorCode::ToolFailed,
                format!(
                    "the inputs of `{}` are over {} KB together; point `{key}` at something shorter",
                    doc.name,
                    STATE_MAX_BYTES / 1024
                ),
            );
        }
        texts.push((key.clone(), text));
    }

    let state = eval::state_of(&texts);
    match client.evaluate(&state, &doc.questions, cancel).await {
        Ok(answers) => {
            let composed = eval::compose(doc, answers);
            let summary = format!(
                "{}: {} route(s), {} escalation(s)",
                doc.name,
                composed.routes.len(),
                composed.escalations.len()
            );
            answered(name, summary, &composed).with_bytes_in(total as u64)
        }
        Err(DecisionError::Cancelled) => Produced::cancelled(name, "the eval was cancelled"),
        Err(err) => Produced::failed(name, err.code(), format!("{err}")),
    }
}

/// Asks model-written questions about a model-written state.
pub(crate) async fn run_ask(
    configured: Option<&DecisionClient>,
    state: &Value,
    questions: &[Question],
    cancel: &CancellationToken,
) -> Produced {
    let name = tool::JEV_ASK;
    let Some(client) = configured else {
        return no_client(name);
    };
    // Checked at parse too; the cap is the payload.
    if let Err(reason) = decision::check_state(state) {
        return Produced::failed(name, ErrorCode::ToolFailed, reason);
    }

    match client.evaluate(state, questions, cancel).await {
        Ok(answers) => {
            let summary = format!(
                "{} answer(s) from {}",
                answers.answers.len(),
                client.model()
            );
            answered(name, summary, &answers)
        }
        Err(DecisionError::Cancelled) => Produced::cancelled(name, "the request was cancelled"),
        Err(err) => Produced::failed(name, err.code(), format!("{err}")),
    }
}
