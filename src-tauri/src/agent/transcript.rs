//! Turning a stored transcript into a model request.
//!
//! One direction only: [`Message`] values come out of the session store and
//! [`ModelRequest`] goes to a provider. Nothing here writes to the store.
//!
//! * **The system message** is rebuilt for every request from pre-rendered
//!   blocks (identity, workspace, memories, skill catalog, world frame, cabinet
//!   digest, folded state), so edits apply on the next request and this module
//!   reads no file. Memories are re-injected every time, so compaction never
//!   loses them (PLAN 7.3).
//! * **Repair**: a cancelled turn can leave a tool call unanswered, which the
//!   API rejects forever after. [`build`] synthesizes the missing answers and
//!   tells the model the call never ran.

use std::path::Path;

use crate::store::{Agent, Message, Role, ToolCallRecord, ToolCallStatus};
use crate::tools::READ_MAX_BYTES;

use super::wire::{ModelRequest, WireMessage, WireToolCall};

/// The standing instructions: what the model is, what the gate does, and how
/// to take a refusal.
const SYSTEM_PROMPT: &str = "\
You are Aegis, a careful assistant running on the user's own computer.

Tools act on the real machine. Every mutating call is shown to the user for \
approval before it runs, and every call is recorded in an audit log. A refusal \
comes back as an ordinary result with `ok: false` and `error.code: \
\"E_DENIED\"` — say what you were trying to do and why, then offer another \
approach. Never repeat a refused call unchanged.

Read before you write. Prefer paths relative to the workspace root. Keep \
replies short, and say plainly when you are not sure.";

/// The `ToolResult` envelope (PLAN 4.3) synthesized for a call that never ran.
const UNANSWERED_ENVELOPE: &str = r#"{"ok":false,"tool":"","content":"","truncated":false,"bytes":0,"meta":{},"error":{"code":"E_CANCELLED","message":"this call never ran: the turn ended before it was executed"}}"#;

/// Everything the system message is built from, as already-rendered text.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The identity the session is bound to (Phase 12).
    pub agent: &'a Agent,
    /// The session's workspace root, or `None` when the folder is gone.
    pub workspace: Option<&'a Path>,
    /// Where this project's commands run (PLAN 7.12), or `None` for this
    /// computer. Placed right after the workspace line.
    pub exec_host: Option<&'a str>,
    /// What this identity has learned (PLAN 7.3, Phase 14), or `None` when it
    /// has learned nothing yet.
    pub memories: Option<&'a str>,
    /// The skill catalog (Phase 13): one line per granted runbook, never its
    /// steps.
    pub skills: Option<&'a str>,
    /// The world's frame (PLAN 7.2), including the absence paragraph when
    /// there is none (PLAN 7.17). A constraint injected by the harness, never
    /// `essence.md` itself.
    pub world: Option<&'a str>,
    /// The shared-workspace digest (PLAN 7.3, Phase 11), or `None` for a folder
    /// that does not use the convention.
    pub shared: Option<&'a str>,
    /// What this session's older turns folded into (PLAN 7.3, Phase 14), or
    /// `None` for a session that has never been compacted.
    pub compacted: Option<&'a str>,
    /// Whether a routine started this unattended run (Phase 16). Repeated here
    /// so it survives the opening message folding.
    pub unattended: bool,
}

/// The system message for a session: who it is, what it knows, where it is,
/// and what has already happened in it.
///
/// Ordered from slowest- to fastest-changing: instructions, identity, workspace
/// (full path, so the model does not guess one), memories, skill catalog, world
/// (above the cabinet it judges), digest, folded state (next to the
/// conversation it replaces). Every appended block is capped (PLAN 7.1).
pub fn system_message(ctx: &Context<'_>) -> String {
    let mut prompt = String::from(SYSTEM_PROMPT);
    let agent = ctx.agent;

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
    // Only "no tools" is stated: an empty `tools` array cannot say it.
    if agent.tools.is_empty() {
        prompt.push_str(
            "\n\nThis identity holds no tools, so nothing you say can touch the \
             machine. Say so if the user asks for anything that would need one.",
        );
    }

    match ctx.workspace {
        Some(path) => {
            prompt.push_str("\n\nThe workspace is: ");
            prompt.push_str(&path.display().to_string());
            prompt.push_str(
                "\nReads inside it happen without asking. Writes, and anything \
                 outside it, are put to the user first.",
            );
        }
        // Folder gone: every call is `E_NO_WORKSPACE` (PLAN 3.2), so say so.
        None => prompt.push_str(
            "\n\nThis session has no workspace folder right now, so no tool can \
             run. Say so if the user asks for anything that would need one.",
        ),
    }

    // Where commands over that folder run (PLAN 7.12); absent for this computer.
    if let Some(host) = ctx.exec_host {
        prompt.push_str("\n\n");
        prompt.push_str(host);
    }

    prompt.push_str(&format!(
        "\n\nA single `fs_read` returns at most {} KB.",
        READ_MAX_BYTES / 1024
    ));

    // Appended in the documented order, and each only when there is one. A
    // loop rather than five `if let`s because the order *is* the rule, and a
    // list of five names is harder to reorder by accident than five blocks.
    for block in [
        ctx.memories,
        ctx.skills,
        ctx.world,
        ctx.shared,
        ctx.compacted,
    ]
    .into_iter()
    .flatten()
    {
        prompt.push_str("\n\n");
        prompt.push_str(block);
    }

    prompt
}

/// Builds the request for the next round of a turn.
///
/// `history` is what still reaches the model, oldest first (the caller applies
/// [`compact::tail`](crate::compact::tail)). Stored `system` messages are
/// dropped in favour of the fresh one.
pub fn build(
    model: &str,
    ctx: &Context<'_>,
    history: &[Message],
    tools: Vec<serde_json::Value>,
) -> ModelRequest {
    let mut messages = vec![WireMessage::System {
        content: system_message(ctx),
    }];

    for message in history {
        match message.role {
            Role::System => {}
            Role::User => messages.push(WireMessage::User {
                content: message.text.clone(),
            }),
            Role::Tool => messages.push(WireMessage::Tool {
                // An id-less `tool` message is rejected by the API; keep its
                // content as plain text instead of dropping it.
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
                        .map(|call| {
                            let mut wire =
                                WireToolCall::new(&call.call_id, &call.tool, &call.args_json);
                            wire.thought_signature = call.thought_signature.clone();
                            wire
                        })
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
/// Scans the whole history, so a misordered transcript gets no duplicate
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

/// Whether a call's status says it will never be answered (for cleanup;
/// [`build`] ignores status).
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
            instructions: "File what you find in .aegis/decisions/DECISIONS.md.".to_owned(),
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
            thought_signature: None,
        }
    }

    /// A workspace path to point at. A `PathBuf` in the caller's frame rather
    /// than a temporary, because [`Context`] borrows it.
    fn root() -> PathBuf {
        PathBuf::from("/w")
    }

    /// An identity in a workspace and nothing else: what most of these
    /// assertions are about.
    fn ctx<'a>(agent: &'a Agent, workspace: Option<&'a Path>) -> Context<'a> {
        Context {
            agent,
            workspace,
            exec_host: None,
            memories: None,
            skills: None,
            world: None,
            shared: None,
            compacted: None,
            unattended: false,
        }
    }

    #[test]
    fn the_system_message_names_the_workspace() {
        let here = PathBuf::from("/home/p/work");
        let prompt = system_message(&ctx(&assistant(), Some(&here)));

        assert!(prompt.contains("/home/p/work"), "{prompt}");
        assert!(
            prompt.contains("E_DENIED"),
            "the refusal contract is stated"
        );
        assert!(prompt.contains("256 KB"), "the read cap is stated");
    }

    #[test]
    fn a_session_without_a_workspace_is_told_so() {
        let prompt = system_message(&ctx(&assistant(), None));

        assert!(prompt.contains("no workspace folder"), "{prompt}");
        assert!(!prompt.contains("Reads inside it"), "{prompt}");
    }

    /// The built-in identity is the assistant of Phases 5–11, named. If it
    /// changed the message, every session written before Phase 12 would start
    /// behaving differently for a migration nobody asked for.
    #[test]
    fn the_default_identity_leaves_the_message_exactly_as_it_was() {
        let prompt = system_message(&ctx(&assistant(), Some(&root())));

        assert!(!prompt.contains("You are working as"), "{prompt}");
        assert!(!prompt.contains("holds no tools"), "{prompt}");
        assert!(prompt.starts_with(SYSTEM_PROMPT), "{prompt}");
    }

    /// An identity is who the model is, so it comes before the facts of the
    /// session — and before anything that changes between two turns.
    #[test]
    fn an_identity_names_itself_before_the_session_facts() {
        let prompt = system_message(&ctx(&reviewer(), Some(&root())));

        let identity = prompt.find("Reviewer").expect("the identity is named");
        let workspace = prompt.find("The workspace is").expect("and the folder");
        assert!(identity < workspace, "{prompt}");

        assert!(prompt.contains("reviews changes"), "the role is stated");
        assert!(
            prompt.contains(".aegis/decisions/DECISIONS.md"),
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
            system_message(&ctx(&none, Some(&root()))).contains("holds no tools"),
            "an identity that cannot act should not discover it one refusal at a time"
        );

        let some = system_message(&ctx(&reviewer(), Some(&root())));
        assert!(
            !some.contains("holds no tools"),
            "an identity with tools has nothing to say about them: {some}"
        );
    }

    /// The catalog is standing context and the body never is (PLAN 7.6). It
    /// also sits before the shared state, because it changes when somebody
    /// writes a runbook and the state changes on every request.
    #[test]
    fn the_skill_catalog_is_carried_and_the_runbook_is_not() {
        let catalog = "Skills you may run.\n\n- `inbox.triage` (v1, this workspace) — sorts an \
                       item into the board. Calls fs_read, fs_write.";
        let reviewer = reviewer();
        let root = root();
        let prompt = system_message(&Context {
            skills: Some(catalog),
            shared: Some(".aegis/status/STATUS.md:\nquiet"),
            ..ctx(&reviewer, Some(&root))
        });

        assert!(prompt.contains("inbox.triage"), "{prompt}");
        let skills = prompt.find("Skills you may run").expect("the catalog");
        let state = prompt.find(".aegis/status/STATUS.md").expect("the state");
        assert!(skills < state, "{prompt}");

        // And an identity granted none is left exactly where Phase 12 left it.
        let without = system_message(&ctx(&reviewer, Some(&root)));
        assert!(!without.contains("Skills you may run"), "{without}");
    }

    /// The shared state is appended after the standing instructions, not woven
    /// into them: the policy contract must read the same whether or not the
    /// workspace uses the convention (PLAN 7.3, Phase 11).
    #[test]
    fn the_shared_state_is_appended_and_changes_nothing_before_it() {
        let assistant = assistant();
        let root = root();
        let plain = system_message(&ctx(&assistant, Some(&root)));
        let shared = system_message(&Context {
            shared: Some(".aegis/status/STATUS.md:\nquiet"),
            ..ctx(&assistant, Some(&root))
        });

        assert!(shared.starts_with(&plain), "{shared}");
        assert!(
            shared.ends_with(".aegis/status/STATUS.md:\nquiet"),
            "{shared}"
        );
    }

    /// The appended blocks keep their order: slowest-changing first, world above
    /// cabinet, folded state last.
    #[test]
    fn the_appended_blocks_are_in_the_documented_order() {
        let reviewer = reviewer();
        let root = root();
        let prompt = system_message(&Context {
            memories: Some("What you have learned. \n- (preference) answers in French"),
            skills: Some("Skills you may run.\n\n- `inbox.triage`"),
            world: Some("This workspace has a world."),
            shared: Some(".aegis/status/STATUS.md:\nquiet"),
            compacted: Some("Earlier in this session, folded to state."),
            unattended: false,
            ..ctx(&reviewer, Some(&root))
        });

        let at = |needle: &str| {
            prompt
                .find(needle)
                .unwrap_or_else(|| panic!("{needle}: {prompt}"))
        };
        let identity = at("Reviewer");
        let memories = at("What you have learned");
        let skills = at("Skills you may run");
        let world = at("This workspace has a world");
        let shared = at(".aegis/status/STATUS.md");
        let folded = at("folded to state");

        assert!(identity < memories, "{prompt}");
        assert!(memories < skills, "{prompt}");
        assert!(skills < world, "{prompt}");
        assert!(world < shared, "{prompt}");
        assert!(shared < folded, "{prompt}");
    }

    /// With no memories and no compaction, the message is unchanged from
    /// Phase 13.
    #[test]
    fn nothing_learned_and_nothing_folded_leaves_the_message_as_it_was() {
        let reviewer = reviewer();
        let root = root();
        let prompt = system_message(&ctx(&reviewer, Some(&root)));

        assert!(!prompt.contains("What you have learned"), "{prompt}");
        assert!(!prompt.contains("folded to state"), "{prompt}");
        assert!(
            !prompt.contains("world"),
            "a workspace with no constitution is not nagged into one: {prompt}"
        );
    }

    #[test]
    fn a_conversation_becomes_the_documented_message_sequence() {
        let history = vec![
            Message::user("list the files"),
            Message::assistant("", vec![call("call_1")]),
            Message::tool("call_1", r#"{"ok":true}"#),
            Message::assistant("Three files.", Vec::new()),
        ];

        let request = build("m", &ctx(&assistant(), Some(&root())), &history, Vec::new());

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

        let request = build("m", &ctx(&assistant(), None), &history, Vec::new());

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

        let request = build("m", &ctx(&assistant(), None), &history, Vec::new());
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

        let fresh = PathBuf::from("/new");
        let request = build("m", &ctx(&assistant(), Some(&fresh)), &history, Vec::new());

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

        let request = build("m", &ctx(&assistant(), None), &history, Vec::new());
        assert_eq!(request.messages.len(), 1, "only the system message remains");
    }

    #[test]
    fn the_tools_array_is_carried_through_untouched() {
        let tools = vec![json!({ "type": "function", "function": { "name": "fs_list" } })];
        let request = build("m", &ctx(&assistant(), None), &[], tools.clone());

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
