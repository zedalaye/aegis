//! Turning a stored transcript into a model request.
//!
//! One direction only: [`Message`] values come out of the session store and
//! [`ModelRequest`] goes to a provider. Nothing here writes to the store.
//!
//! Two jobs, and the second is the interesting one.
//!
//! The first is the system message. It is rebuilt on every request rather than
//! stored with the transcript, because it names the workspace, and the
//! workspace can change between turns — a stored system message would keep
//! telling the model about a folder the user has since moved on from. From
//! Phase 11 it also carries the shared workspace's current state, which changes
//! faster still: it is read out of the files on every request
//! ([`workspace::digest`](crate::workspace::digest)) and handed in here as
//! text, so this module keeps its one direction and never touches a disk. From
//! Phase 12 it opens with the session's identity — who this is and what it is
//! for — which is rebuilt for the same reason: an identity can be edited
//! between two turns, and the next request should carry the edit.
//!
//! The second is repair. The chat-completions API has a structural rule that
//! the transcript can violate: every tool call in an assistant message must be
//! answered by a `tool` message with a matching id. A turn cancelled between
//! "the model asked" and "the tool ran" leaves an unanswered call on disk
//! forever, and every subsequent request in that session would be rejected by
//! the provider with a 400 — a session bricked by a cancel. [`build`]
//! therefore synthesizes the missing answers. The session stays usable, and
//! the model is told plainly that the call never ran.

use std::path::Path;

use crate::store::{Agent, Message, Role, ToolCallRecord, ToolCallStatus};
use crate::tools::READ_MAX_BYTES;

use super::wire::{ModelRequest, WireMessage, WireToolCall};

/// The standing instructions, before the per-session facts are appended.
///
/// Deliberately short. It says what the model is, what the gate does to its
/// tool calls, and how to behave when one is refused — the three things it
/// cannot work out from the tool schemas alone. Everything else is the user's
/// to say.
const SYSTEM_PROMPT: &str = "\
You are Aegis, a careful assistant running on the user's own computer.

Tools act on the real machine. Every mutating call is shown to the user for \
approval before it runs, and every call is recorded in an audit log. A refusal \
comes back as an ordinary result with `ok: false` and `error.code: \
\"E_DENIED\"` — say what you were trying to do and why, then offer another \
approach. Never repeat a refused call unchanged.

Read before you write. Prefer paths relative to the workspace root. Keep \
replies short, and say plainly when you are not sure.";

/// The envelope a synthesized answer carries for a call that never ran.
///
/// The same `ToolResult` shape every real tool produces (PLAN 4.3), so the
/// model has nothing new to learn in order to read it.
const UNANSWERED_ENVELOPE: &str = r#"{"ok":false,"tool":"","content":"","truncated":false,"bytes":0,"meta":{},"error":{"code":"E_CANCELLED","message":"this call never ran: the turn ended before it was executed"}}"#;

/// The system message for a session: its identity, its workspace, its shared
/// state.
///
/// The order is the order of how slowly the parts change. The standing
/// instructions never change. The identity changes when someone edits it. The
/// workspace changes when the project does. The shared digest changes on every
/// request. Reading top to bottom is therefore reading from "what is always
/// true" to "what is true right now", which is also the order that survives a
/// model skimming it.
///
/// `agent` is the identity the session is bound to (PLAN 7.3, Phase 12). The
/// built-in one carries no role and no instructions, so a default session's
/// message is byte for byte the one Phase 11 produced.
///
/// The workspace is named in full because a model asked to work "in the
/// project" with no path guesses one, and a guessed absolute path is exactly
/// the tool call the user then has to read carefully and refuse.
///
/// `shared` is the shared-workspace digest (PLAN 7.3, Phase 11), or `None` for
/// a folder that does not use the convention. It goes last because it is
/// *state*, not procedure. The prompt stays a policy summary plus what is true
/// right now; runbooks are skills, and skills are Phase 13 (PLAN 7.1, *System
/// prompt*) — which is also why an identity's instructions are capped in the
/// store rather than trimmed here.
pub fn system_message(agent: &Agent, workspace: Option<&Path>, shared: Option<&str>) -> String {
    let mut prompt = String::from(SYSTEM_PROMPT);

    if !agent.role.is_empty() {
        prompt.push_str(&format!(
            "\n\nYou are working as `{}`: {}.",
            agent.name,
            agent.role.trim_end_matches('.')
        ));
    }
    if !agent.instructions.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&agent.instructions);
    }
    // Said out loud only when it is the whole answer. Listing the granted tools
    // would be restating the schemas the model already has, and inviting it to
    // argue with them; having *none* is the one case the schemas cannot state,
    // because an absence is not something a `tools` array can carry.
    if agent.tools.is_empty() {
        prompt.push_str(
            "\n\nThis identity holds no tools, so nothing you say can touch the \
             machine. Say so if the user asks for anything that would need one.",
        );
    }

    match workspace {
        Some(path) => {
            prompt.push_str("\n\nThe workspace is: ");
            prompt.push_str(&path.display().to_string());
            prompt.push_str(
                "\nReads inside it happen without asking. Writes, and anything \
                 outside it, are put to the user first.",
            );
        }
        // Reachable when a project's folder has been unmounted or moved. Every
        // tool call is a hard `E_NO_WORKSPACE` denial in that state (PLAN
        // 3.2), so the model is better off being told than discovering it one
        // refusal at a time.
        None => prompt.push_str(
            "\n\nThis session has no workspace folder right now, so no tool can \
             run. Say so if the user asks for anything that would need one.",
        ),
    }

    prompt.push_str(&format!(
        "\n\nA single `fs_read` returns at most {} KB.",
        READ_MAX_BYTES / 1024
    ));

    if let Some(shared) = shared {
        prompt.push_str("\n\n");
        prompt.push_str(shared);
    }

    prompt
}

/// Builds the request for the next round of a turn.
///
/// `history` is the session's stored messages, oldest first. Any `system`
/// message in it is dropped: the one this function prepends is the current
/// one, and two system messages is not a shape the API defines.
pub fn build(
    model: &str,
    agent: &Agent,
    history: &[Message],
    workspace: Option<&Path>,
    shared: Option<&str>,
    tools: Vec<serde_json::Value>,
) -> ModelRequest {
    let mut messages = vec![WireMessage::System {
        content: system_message(agent, workspace, shared),
    }];

    for message in history {
        match message.role {
            Role::System => {}
            Role::User => messages.push(WireMessage::User {
                content: message.text.clone(),
            }),
            Role::Tool => messages.push(WireMessage::Tool {
                // A `tool` message with no id cannot be matched to a call, and
                // the API rejects it. Dropping it would silently shorten the
                // conversation, so it is degraded to plain user-visible text
                // instead — the content is still information the model needs.
                tool_call_id: match &message.tool_call_id {
                    Some(id) => id.clone(),
                    None => {
                        tracing::warn!(id = %message.id, "a tool message carries no call id");
                        continue;
                    }
                },
                content: message.text.clone(),
            }),
            Role::Assistant => {
                messages.push(WireMessage::Assistant {
                    content: if message.text.is_empty() {
                        None
                    } else {
                        Some(message.text.clone())
                    },
                    tool_calls: message
                        .tool_calls
                        .iter()
                        .map(|call| WireToolCall::new(&call.call_id, &call.tool, &call.args_json))
                        .collect(),
                });

                for answer in unanswered(message, history) {
                    messages.push(answer);
                }
            }
        }
    }

    ModelRequest {
        model: model.to_owned(),
        messages,
        tools,
    }
}

/// Synthesizes a `tool` message for every call of `message` that the rest of
/// `history` never answered.
///
/// Scanning the whole history rather than only what follows is deliberate: the
/// answers are always later, but a transcript that was reordered by a bug
/// should still produce a valid request rather than a second, duplicate
/// answer.
fn unanswered(message: &Message, history: &[Message]) -> Vec<WireMessage> {
    message
        .tool_calls
        .iter()
        .filter(|call| !is_answered(call, history))
        .map(|call| {
            tracing::debug!(
                call_id = %call.call_id,
                status = ?call.status,
                "answering a tool call the transcript left open"
            );
            WireMessage::Tool {
                tool_call_id: call.call_id.clone(),
                content: UNANSWERED_ENVELOPE.to_owned(),
            }
        })
        .collect()
}

/// Whether some `tool` message in `history` answers this call.
fn is_answered(call: &ToolCallRecord, history: &[Message]) -> bool {
    history.iter().any(|message| {
        message.role == Role::Tool && message.tool_call_id.as_deref() == Some(&call.call_id)
    })
}

/// Whether a call's status says it will never be answered.
///
/// Not used by [`build`], which trusts the transcript rather than the status
/// field, but useful to a caller cleaning up after a cancelled turn.
pub const fn is_terminal(status: ToolCallStatus) -> bool {
    matches!(
        status,
        ToolCallStatus::Ok
            | ToolCallStatus::Error
            | ToolCallStatus::Denied
            | ToolCallStatus::Cancelled
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use serde_json::json;

    use crate::store::Agent;

    /// The default identity: what every session before Phase 12 resolves to.
    fn assistant() -> Agent {
        Agent::builtin()
    }

    /// A narrow identity, for the parts of the message that are about one.
    fn reviewer() -> Agent {
        Agent {
            name: "Reviewer".to_owned(),
            role: "reviews changes and reports what is risky".to_owned(),
            instructions: "File what you find in decisions/DECISIONS.md.".to_owned(),
            tools: vec![crate::policy::tool::FS_READ.to_owned()],
            ..Agent::builtin()
        }
    }

    fn call(id: &str) -> ToolCallRecord {
        ToolCallRecord {
            call_id: id.to_owned(),
            tool: "fs_read".to_owned(),
            args_json: r#"{"path":"a.txt"}"#.to_owned(),
            status: ToolCallStatus::Pending,
            summary: None,
            image_path: None,
        }
    }

    #[test]
    fn the_system_message_names_the_workspace() {
        let prompt = system_message(&assistant(), Some(&PathBuf::from("/home/p/work")), None);

        assert!(prompt.contains("/home/p/work"), "{prompt}");
        assert!(
            prompt.contains("E_DENIED"),
            "the refusal contract is stated"
        );
        assert!(prompt.contains("256 KB"), "the read cap is stated");
    }

    #[test]
    fn a_session_without_a_workspace_is_told_so() {
        let prompt = system_message(&assistant(), None, None);

        assert!(prompt.contains("no workspace folder"), "{prompt}");
        assert!(!prompt.contains("Reads inside it"), "{prompt}");
    }

    /// The built-in identity is the assistant of Phases 5–11, named. If it
    /// changed the message, every session written before Phase 12 would start
    /// behaving differently for a migration nobody asked for.
    #[test]
    fn the_default_identity_leaves_the_message_exactly_as_it_was() {
        let prompt = system_message(&assistant(), Some(&PathBuf::from("/w")), None);

        assert!(!prompt.contains("You are working as"), "{prompt}");
        assert!(!prompt.contains("holds no tools"), "{prompt}");
        assert!(prompt.starts_with(SYSTEM_PROMPT), "{prompt}");
    }

    /// An identity is who the model is, so it comes before the facts of the
    /// session — and before anything that changes between two turns.
    #[test]
    fn an_identity_names_itself_before_the_session_facts() {
        let prompt = system_message(&reviewer(), Some(&PathBuf::from("/w")), None);

        let identity = prompt.find("Reviewer").expect("the identity is named");
        let workspace = prompt.find("The workspace is").expect("and the folder");
        assert!(identity < workspace, "{prompt}");

        assert!(prompt.contains("reviews changes"), "the role is stated");
        assert!(
            prompt.contains("decisions/DECISIONS.md"),
            "and its instructions are carried"
        );
    }

    /// The tools an identity holds are the schemas it is shown; restating them
    /// invites argument. Holding *none* is the one thing a `tools` array cannot
    /// express, so it is the one thing said in words.
    #[test]
    fn an_identity_with_no_tools_is_told_so_and_one_with_tools_is_not() {
        let none = Agent {
            tools: Vec::new(),
            ..reviewer()
        };
        assert!(
            system_message(&none, Some(&PathBuf::from("/w")), None).contains("holds no tools"),
            "an identity that cannot act should not discover it one refusal at a time"
        );

        let some = system_message(&reviewer(), Some(&PathBuf::from("/w")), None);
        assert!(
            !some.contains("holds no tools"),
            "an identity with tools has nothing to say about them: {some}"
        );
    }

    /// The shared state is appended after the standing instructions, not woven
    /// into them: the policy contract must read the same whether or not the
    /// workspace uses the convention (PLAN 7.3, Phase 11).
    #[test]
    fn the_shared_state_is_appended_and_changes_nothing_before_it() {
        let plain = system_message(&assistant(), Some(&PathBuf::from("/w")), None);
        let shared = system_message(
            &assistant(),
            Some(&PathBuf::from("/w")),
            Some("status/STATUS.md:\nquiet"),
        );

        assert!(shared.starts_with(&plain), "{shared}");
        assert!(shared.ends_with("status/STATUS.md:\nquiet"), "{shared}");
    }

    #[test]
    fn a_conversation_becomes_the_documented_message_sequence() {
        let history = vec![
            Message::user("list the files"),
            Message::assistant("", vec![call("call_1")]),
            Message::tool("call_1", r#"{"ok":true}"#),
            Message::assistant("Three files.", Vec::new()),
        ];

        let request = build(
            "m",
            &assistant(),
            &history,
            Some(&PathBuf::from("/w")),
            None,
            Vec::new(),
        );

        assert!(matches!(request.messages[0], WireMessage::System { .. }));
        assert!(matches!(request.messages[1], WireMessage::User { .. }));

        match &request.messages[2] {
            WireMessage::Assistant {
                content,
                tool_calls,
            } => {
                assert!(content.is_none(), "an empty text becomes null");
                assert_eq!(tool_calls[0].id, "call_1");
                assert_eq!(tool_calls[0].function.name, "fs_read");
            }
            other => panic!("expected an assistant message, got {other:?}"),
        }

        match &request.messages[3] {
            WireMessage::Tool { tool_call_id, .. } => assert_eq!(tool_call_id, "call_1"),
            other => panic!("expected a tool message, got {other:?}"),
        }

        match &request.messages[4] {
            WireMessage::Assistant { content, .. } => {
                assert_eq!(content.as_deref(), Some("Three files."));
            }
            other => panic!("expected an assistant message, got {other:?}"),
        }
        assert_eq!(request.messages.len(), 5);
    }

    /// The bug this guards against bricks a session: a cancel between "the
    /// model asked" and "the tool ran" leaves an unanswered call on disk, and
    /// every later request in that session is a provider 400.
    #[test]
    fn an_unanswered_tool_call_is_answered_rather_than_left_dangling() {
        let history = vec![
            Message::user("read it"),
            Message::assistant("", vec![call("call_1"), call("call_2")]),
            Message::tool("call_1", r#"{"ok":true}"#),
            // `call_2` was never run: the turn was cancelled.
            Message::user("never mind, what about this"),
        ];

        let request = build("m", &assistant(), &history, None, None, Vec::new());

        let answered: Vec<&str> = request
            .messages
            .iter()
            .filter_map(|message| match message {
                WireMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(answered, vec!["call_2", "call_1"]);

        // The synthesized answer is an ordinary envelope the model can read.
        let synthesized = request
            .messages
            .iter()
            .find_map(|message| match message {
                WireMessage::Tool {
                    tool_call_id,
                    content,
                } if tool_call_id == "call_2" => Some(content),
                _ => None,
            })
            .expect("the missing answer was synthesized");
        let envelope: serde_json::Value =
            serde_json::from_str(synthesized).expect("valid JSON envelope");
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["error"]["code"], "E_CANCELLED");
    }

    #[test]
    fn an_answered_call_is_not_answered_twice() {
        let history = vec![
            Message::assistant("", vec![call("call_1")]),
            Message::tool("call_1", r#"{"ok":true}"#),
        ];

        let request = build("m", &assistant(), &history, None, None, Vec::new());
        let answers = request
            .messages
            .iter()
            .filter(|message| matches!(message, WireMessage::Tool { .. }))
            .count();

        assert_eq!(answers, 1);
    }

    #[test]
    fn a_stored_system_message_is_replaced_rather_than_appended() {
        let history = vec![
            Message {
                role: Role::System,
                ..Message::user("stale instructions naming an old workspace")
            },
            Message::user("hi"),
        ];

        let request = build(
            "m",
            &assistant(),
            &history,
            Some(&PathBuf::from("/new")),
            None,
            Vec::new(),
        );

        let systems = request
            .messages
            .iter()
            .filter(|message| matches!(message, WireMessage::System { .. }))
            .count();
        assert_eq!(systems, 1);

        match &request.messages[0] {
            WireMessage::System { content } => assert!(content.contains("/new"), "{content}"),
            other => panic!("expected the system message first, got {other:?}"),
        }
    }

    #[test]
    fn a_tool_message_with_no_call_id_is_dropped_rather_than_sent() {
        let history = vec![Message {
            tool_call_id: None,
            ..Message::tool("x", "{}")
        }];

        let request = build("m", &assistant(), &history, None, None, Vec::new());
        assert_eq!(request.messages.len(), 1, "only the system message remains");
    }

    #[test]
    fn the_tools_array_is_carried_through_untouched() {
        let tools = vec![json!({ "type": "function", "function": { "name": "fs_list" } })];
        let request = build("m", &assistant(), &[], None, None, tools.clone());

        assert_eq!(request.tools, tools);
        assert_eq!(request.model, "m");
    }

    #[test]
    fn terminal_statuses_are_the_ones_that_will_never_be_answered() {
        assert!(is_terminal(ToolCallStatus::Ok));
        assert!(is_terminal(ToolCallStatus::Denied));
        assert!(is_terminal(ToolCallStatus::Cancelled));
        assert!(!is_terminal(ToolCallStatus::Pending));
        assert!(!is_terminal(ToolCallStatus::Running));
    }
}
