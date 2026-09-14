//! Calling a tool that lives in another process (PLAN 7.3, Phase 18).
//!
//! Passes approved arguments to the connector and wraps the answer in an
//! envelope (PLAN 4.3); all checks happened earlier in policy.
//!
//! * `isError` and transport failures are both `ok: false`;
//!   `meta.reached_the_server` tells them apart.
//! * Non-text content is described, not inlined
//!   ([`mcp::read_answer`](crate::mcp::read_answer)).

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::error::ErrorCode;
use crate::mcp::{self, Connectors};
use crate::store::connectors;

use super::Produced;

/// Runs one connector call.
///
/// `name` is the full `git__status` form, split only here.
pub(crate) async fn call(
    connectors_roster: &Connectors,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Produced {
    let Some((connector, tool)) = connectors::split_tool_name(name) else {
        // Unreachable through the gate: policy parsed this name into a
        // connector call, which means it split. Answered rather than panicked,
        // because a tool that took the turn down would be worse than one that
        // says it does not know what it was asked.
        return Produced::failed(
            name,
            ErrorCode::ToolFailed,
            format!("`{name}` does not name a connector tool"),
        );
    };

    let bytes_in = args.to_string().len() as u64;

    // Raced against the turn's own token, for the reason `shell_exec` is: a
    // connector call can take two minutes, and Stop has to mean stopped rather
    // than "stopped after this one". The connector keeps running — it is a
    // long-lived process and this was one request on it — but nothing waits on
    // the answer, and the next call gets a fresh id.
    let answered = tokio::select! {
        // Biased, so a token that is already cancelled wins rather than racing:
        // "stopped" and "answered" are different words in a transcript, and
        // which one a person reads should not depend on a scheduler.
        biased;

        () = cancel.cancelled() => {
            return Produced::cancelled(
                name,
                format!("the call to `{tool}` was stopped before the `{connector}` connector answered"),
            );
        }
        answered = connectors_roster.call(connector, tool, args) => answered,
    };

    match answered {
        Ok(result) => {
            let answer = mcp::read_answer(&result);
            let meta = json!({
                "connector": connector,
                "tool": tool,
                // True whenever the request completed, whatever the server
                // said about it. It is the fact that separates "call it
                // differently" from "this connector is down".
                "reached_the_server": true,
            });

            if answer.failed {
                let message = if answer.text.trim().is_empty() {
                    format!("`{tool}` failed, and the `{connector}` connector did not say why")
                } else {
                    answer.text.clone()
                };
                return Produced::failed(name, ErrorCode::ToolFailed, message)
                    .with_bytes_in(bytes_in);
            }

            let summary = summarize(connector, tool, &answer);
            Produced::ok(
                name,
                summary,
                answer.text,
                answer.bytes,
                answer.truncated,
                meta,
            )
            .with_bytes_in(bytes_in)
        }
        // The request never completed: the process is gone, it did not answer
        // inside the ceiling, or there is no such connector any more. The
        // message says which, and it says the one thing the model can act on —
        // that starting a connector is not something it can do.
        Err(why) => Produced::failed(name, ErrorCode::ToolFailed, why).with_bytes_in(bytes_in),
    }
}

/// The one line the transcript and the audit row show.
fn summarize(connector: &str, tool: &str, answer: &mcp::Answer) -> String {
    if answer.bytes == 0 {
        return format!("{connector} · {tool}: nothing came back");
    }
    if answer.truncated {
        return format!(
            "{connector} · {tool}: {} bytes, truncated to {}",
            answer.bytes,
            answer.text.len()
        );
    }
    format!("{connector} · {tool}: {} bytes", answer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_call_to_a_connector_nobody_installed_is_an_envelope_not_a_panic() {
        let roster = Connectors::new();
        let produced = call(
            &roster,
            "git__status",
            &json!({}),
            &CancellationToken::new(),
        )
        .await;

        assert!(!produced.result.ok);
        let error = produced.result.error.expect("a reason");
        assert_eq!(error.code, ErrorCode::ToolFailed);
        // The model is told the one thing it can act on: it cannot fix this.
        assert!(error.message.contains("Settings"), "{}", error.message);
    }

    #[tokio::test]
    async fn a_cancelled_call_is_cancelled_rather_than_failed() {
        let roster = Connectors::new();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let produced = call(&roster, "git__status", &json!({}), &cancel).await;
        assert_eq!(
            produced.result.error.expect("a reason").code,
            ErrorCode::Cancelled
        );
    }

    #[test]
    fn the_summary_says_how_much_came_back() {
        let answer = mcp::Answer {
            text: "on branch main".to_owned(),
            failed: false,
            bytes: 14,
            truncated: false,
        };
        assert_eq!(
            summarize("git", "status", &answer),
            "git · status: 14 bytes"
        );
    }
}
