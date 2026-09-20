//! The `jev_eval` and `jev_ask` rows of the table (PLAN 7.18).

use std::collections::BTreeMap;

use super::*;
use crate::agent::decision::Question;

/// PLAN 7.18: the eval file owns the questions; the model names it and,
/// at most, other files for its declared inputs.
pub(super) fn eval_call(
    workspace: &Path,
    name: String,
    inputs: BTreeMap<String, String>,
) -> Result<Decision, Decision> {
    let doc = match eval::load(workspace, &name) {
        Loaded::Ready(doc) => doc,
        Loaded::Missing { proposed: true } => {
            return Err(Decision::deny(
                ErrorCode::Denied,
                format!(
                    "`{name}` is only proposed: `.aegis/evals/{name}/PROPOSAL.yml` has \
                     not been applied, and a proposal does not run. A person applies it \
                     by copying it onto `eval.yml`"
                ),
            ))
        }
        Loaded::Missing { proposed: false } => {
            return Err(Decision::deny(
                ErrorCode::ToolFailed,
                format!(
                    "there is no signed eval `{name}` in this workspace \
                     (`.aegis/evals/{name}/eval.yml`)"
                ),
            ))
        }
        Loaded::Broken(problem) => {
            return Err(Decision::deny(
                ErrorCode::ToolFailed,
                format!("the eval `{name}` cannot run: {problem}"),
            ))
        }
    };

    if let Some(unknown) = inputs.keys().find(|key| !doc.inputs.contains_key(*key)) {
        return Err(Decision::deny(
            ErrorCode::ToolFailed,
            format!(
                "`{unknown}` is not an input of `{name}`; it takes {}",
                doc.inputs.keys().cloned().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    let mut paths = doc.inputs.clone();
    paths.extend(inputs);

    let mut resolved = Vec::with_capacity(paths.len());
    let mut shown = Vec::with_capacity(paths.len());
    let mut sensitive = false;
    for (key, raw) in &paths {
        let target = resolve(workspace, raw)?;
        if !target.inside {
            return Err(Decision::deny(
                ErrorCode::PathOutsideWorkspace,
                format!("input `{key}` (`{raw}`) is outside the workspace"),
            ));
        }
        sensitive |= is_sensitive(workspace, &target);
        shown.push(format!("{key}: {}", relative_label(workspace, &target)));
        resolved.push((key.clone(), target.path));
    }

    // A credential-shaped input is asked about each time.
    let grant = (!sensitive).then(|| Grant::JevEval { name: name.clone() });
    let detail = ApprovalDetail::JevEval {
        name: name.clone(),
        inputs: shown,
        questions: doc.questions.iter().map(|q| q.id.clone()).collect(),
    };
    Ok(ask(
        ResolvedCall::JevEval {
            eval: doc,
            inputs: resolved,
        },
        AskRequest {
            tool: tool::JEV_EVAL.to_owned(),
            risk: Risk::High,
            title: "Run a project eval",
            summary: format!("{name} · {} input file(s) to TypeSafe", paths.len()),
            detail,
            scope_label: scope_label(grant.as_ref()),
            grant,
            reason: if sensitive {
                "an input's name suggests it holds a credential, and its content would \
                 leave this machine for TypeSafe"
                    .to_owned()
            } else {
                "the named workspace files leave this machine for TypeSafe".to_owned()
            },
        },
    ))
}

/// The soupape: always an ask, whatever the key (PLAN 7.18).
pub(super) fn ask_call(
    ctx: &PolicyCtx<'_>,
    state: serde_json::Value,
    questions: Vec<Question>,
) -> Result<Decision, Decision> {
    let count = questions.len();
    let pretty = serde_json::to_string_pretty(&state).unwrap_or_default();
    let grant = Grant::JevAsk;
    let detail = ApprovalDetail::JevAsk {
        model: ctx.decision_model.map(str::to_owned),
        question_count: u32::try_from(count).unwrap_or(u32::MAX),
        questions: questions.iter().map(|q| q.line()).collect(),
        state_preview: preview(&pretty).unwrap_or_default(),
    };
    Ok(ask(
        ResolvedCall::JevAsk { state, questions },
        AskRequest {
            tool: tool::JEV_ASK.to_owned(),
            risk: Risk::High,
            title: "Ask the decision model",
            summary: format!(
                "{count} question{} to TypeSafe",
                if count == 1 { "" } else { "s" }
            ),
            detail,
            scope_label: scope_label(Some(&grant)),
            grant: Some(grant),
            reason: "the state below, written by the model, leaves this machine for \
                     TypeSafe"
                .to_owned(),
        },
    ))
}
