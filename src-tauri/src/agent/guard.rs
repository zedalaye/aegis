//! When a turn should stop calling tools (PLAN 7.16).
//!
//! Three guards, all deterministic, none a permission gate:
//!
//! * **A loop** is the same round fingerprint [`LOOP_STREAK`] times running.
//!   The fingerprint is the tool names and canonical arguments, not the call
//!   ids, so a retry of the same work is visible.
//! * **A ceiling** ([`MAX_TOOL_ROUNDS`]) bounds the bill when the work is
//!   still progressing. It is the same with or without a runbook.
//! * **A budget** is a model-spend cap the last round passed (PLAN 7.26),
//!   decided by the [`Meter`](crate::spend::Meter), not here.
//!
//! Hitting any refuses the pending calls and lets the model have one wrap-up
//! round. The recovery is not "ask the user to continue".

use serde_json::Value;

use crate::error::ErrorCode;

use super::wire::AssembledCall;

/// Tool rounds allowed in one turn. The bill bound, not the loop bound.
///
/// A measured `review.diff` needed about twenty; eight was never measured
/// (`IDEAS.md` § 11). Progress past this still stops — the operator can send
/// another message — but ordinary work should not hit it.
pub const MAX_TOOL_ROUNDS: u32 = 64;

/// Consecutive identical rounds that count as a loop. Two can be a retry
/// after a failed read; three is the model stuck.
pub const LOOP_STREAK: u32 = 3;

/// Why the pending round must not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    /// The same tools with the same arguments, [`LOOP_STREAK`] times.
    Loop,
    /// [`MAX_TOOL_ROUNDS`] rounds already ran in this turn.
    Ceiling,
    /// A model-spend cap is reached (PLAN 7.26).
    Budget,
}

impl Halt {
    /// The envelope code the refused calls carry.
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::Loop => ErrorCode::ToolLoop,
            Self::Ceiling => ErrorCode::TooManyToolRounds,
            Self::Budget => ErrorCode::Budget,
        }
    }

    /// What the model is told. Finish now; do not ask the user to continue.
    pub fn reason(self, skill: Option<&str>) -> String {
        let head = match self {
            Self::Loop => format!(
                "this turn called the same tools with the same arguments {LOOP_STREAK} times in \
                 a row, so it stopped as a loop; do not retry those calls. Finish with what you \
                 have"
            ),
            Self::Ceiling => format!(
                "this turn already ran {MAX_TOOL_ROUNDS} rounds of tools, which is the limit; \
                 finish with what you have"
            ),
            Self::Budget => "this turn reached the model-spend cap it runs under, so no more                              tools run; finish with what you have, in this reply"
                .to_owned(),
        };
        match skill {
            Some(name) => format!(
                "{head}. The `{name}` run stays open until you close it with `skill_return`"
            ),
            None => head,
        }
    }
}

/// The identity of a round: names and arguments, sorted, so parallel call
/// order does not look like progress.
pub fn fingerprint(calls: &[AssembledCall]) -> String {
    let mut parts: Vec<String> = calls.iter().map(call_fingerprint).collect();
    parts.sort();
    parts.join("\n")
}

/// Whether this incoming round must be refused rather than run.
///
/// `executed` is how many rounds already ran. `history` is their fingerprints
/// in order. Loop is judged first so a stuck model is named a loop, not a
/// ceiling, if both could apply.
pub fn halt(executed: u32, history: &[String], incoming: &str) -> Option<Halt> {
    if is_loop(history, incoming) {
        return Some(Halt::Loop);
    }
    if executed >= MAX_TOOL_ROUNDS {
        return Some(Halt::Ceiling);
    }
    None
}

fn call_fingerprint(call: &AssembledCall) -> String {
    let args = match &call.args {
        Ok(value) => canonical(value),
        Err(_) => call.args_json.clone(),
    };
    format!("{}\t{args}", call.name)
}

/// `serde_json::Map` is a `BTreeMap`, so object keys serialize in a stable
/// order. Arrays keep the order the model wrote.
fn canonical(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn is_loop(history: &[String], incoming: &str) -> bool {
    let need = match usize::try_from(LOOP_STREAK.saturating_sub(1)) {
        Ok(n) => n,
        Err(_) => return false,
    };
    if need == 0 {
        return true;
    }
    let n = history.len();
    if n < need {
        return false;
    }
    history[n - need..].iter().all(|seen| seen == incoming)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::wire::AssembledCall;

    fn call(name: &str, args: &str) -> AssembledCall {
        AssembledCall {
            call_id: "x".to_owned(),
            name: name.to_owned(),
            args_json: args.to_owned(),
            args: serde_json::from_str(args).map_err(|err| err.to_string()),
            thought_signature: None,
        }
    }

    #[test]
    fn parallel_call_order_does_not_change_the_fingerprint() {
        let a = call("fs_read", r#"{"path":"a.txt"}"#);
        let b = call("fs_read", r#"{"path":"b.txt"}"#);
        assert_eq!(
            fingerprint(&[a.clone(), b.clone()]),
            fingerprint(&[b, a]),
            "the same pair in either order is one round"
        );
    }

    #[test]
    fn object_key_order_does_not_change_the_fingerprint() {
        let left = call("fs_write", r#"{"path":"a.txt","content":"x"}"#);
        let right = call("fs_write", r#"{"content":"x","path":"a.txt"}"#);
        assert_eq!(fingerprint(&[left]), fingerprint(&[right]));
    }

    #[test]
    fn a_different_path_is_progress() {
        let first = fingerprint(&[call("fs_read", r#"{"path":"a.txt"}"#)]);
        let second = fingerprint(&[call("fs_read", r#"{"path":"b.txt"}"#)]);
        assert_ne!(first, second);
        assert_eq!(halt(1, &[first], &second), None);
    }

    #[test]
    fn two_identical_rounds_are_still_run() {
        let same = fingerprint(&[call("fs_read", r#"{"path":"a.txt"}"#)]);
        assert_eq!(
            halt(1, std::slice::from_ref(&same), &same),
            None,
            "a single retry after a failed read is not a loop"
        );
    }

    #[test]
    fn three_identical_rounds_are_a_loop() {
        let same = fingerprint(&[call("fs_read", r#"{"path":"a.txt"}"#)]);
        assert_eq!(
            halt(2, &[same.clone(), same.clone()], &same),
            Some(Halt::Loop)
        );
    }

    #[test]
    fn a_call_id_is_not_progress() {
        let mut first = call("fs_read", r#"{"path":"a.txt"}"#);
        first.call_id = "call_1".to_owned();
        let mut second = call("fs_read", r#"{"path":"a.txt"}"#);
        second.call_id = "call_2".to_owned();
        assert_eq!(fingerprint(&[first]), fingerprint(&[second]));
    }

    #[test]
    fn the_ceiling_fires_only_once_the_rounds_have_run() {
        let incoming = fingerprint(&[call("fs_read", r#"{"path":"z.txt"}"#)]);
        let history: Vec<String> = (0..MAX_TOOL_ROUNDS)
            .map(|n| format!("fs_read\t{n}"))
            .collect();
        assert_eq!(halt(MAX_TOOL_ROUNDS - 1, &history, &incoming), None);
        assert_eq!(
            halt(MAX_TOOL_ROUNDS, &history, &incoming),
            Some(Halt::Ceiling)
        );
    }

    #[test]
    fn a_loop_is_named_even_if_the_ceiling_would_also_apply() {
        let same = "fs_read\ta.txt".to_owned();
        let history: Vec<String> = (0..MAX_TOOL_ROUNDS).map(|_| same.clone()).collect();
        assert_eq!(
            halt(MAX_TOOL_ROUNDS, &history, &same),
            Some(Halt::Loop),
            "a stuck model is a loop, not a bill"
        );
    }

    #[test]
    fn loop_reason_does_not_ask_the_user_to_continue() {
        let text = Halt::Loop.reason(None);
        assert!(text.contains("loop"), "{text}");
        assert!(
            !text.to_lowercase().contains("continue"),
            "the recovery is finish, not a human continue: {text}"
        );
    }
}
