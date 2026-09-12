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
//! between two turns, and the next request should carry the edit. From Phase
//! 13 it also carries the skill *catalog*, on the same terms and for a
//! stronger reason: the catalog is one line per runbook, and the runbook
//! itself is never here. A system message that carried the steps of every
//! skill would pay for every procedure on every turn, which is the
//! anti-pattern the catalog exists to prevent (PLAN 7.6). From Phase 14 it
//! carries two more: what this identity has learned
//! ([`memories`](crate::store::memories)), and — for a session that has been
//! compacted — the state its older turns folded into
//! ([`compact`](crate::compact)).
//!
//! Those last two are PLAN 7.3's retrieve-after-compact, and it is worth
//! saying why nothing in this tree is named that. The memory block is rebuilt
//! into *every* system message, so it was never in the part that folds: there
//! is nothing to restore, in the same way there is nothing to restore about the
//! workspace digest. What a compaction takes away is conversation. What it
//! leaves standing is what the identity knows.
//!
//! One block is not like the others. When the workspace holds a `world/`
//! (PLAN 7.2 — the missed half of Phase 11), the message also carries that
//! world's *frame*: read the constitution, do not write it, do not reopen the
//! sources it was perceived from. That is a constraint rather than a fact, and
//! it is here rather than in a runbook because a runbook can be skipped and
//! forgetting it is the defect the whole shape exists to prevent. It arrives as
//! rendered text like everything else, so this module still reads no file.
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

/// Everything the system message is built out of, for one request.
///
/// A struct rather than six arguments, for the reason [`Turn`](super::Turn) is
/// one: four of them are optional strings, a call site that transposed two
/// would still compile, and a later phase adding a fifth should not re-thread
/// every signature. Every field is *already rendered* text — this module reads
/// no file and asks no store anything, which is what keeps it a projection with
/// one direction.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The identity the session is bound to (PLAN 7.3, Phase 12).
    ///
    /// The built-in one carries no role and no instructions, so a default
    /// session's message is byte for byte the one Phase 11 produced.
    pub agent: &'a Agent,
    /// The session's workspace root, or `None` when the folder is gone.
    pub workspace: Option<&'a Path>,
    /// Where this project's commands run (PLAN 7.12), already rendered, or
    /// `None` when they run on this computer.
    ///
    /// It sits with the workspace rather than with the slow-changing blocks
    /// below, and directly after it, because it is the same kind of fact and
    /// only makes sense beside it: the folder is where the file tools work, and
    /// this is the machine a command in that folder runs on. A model that has
    /// not been told will reach for `pnpm.cmd` and pass a Windows path as an
    /// argument — two rounds spent discovering one sentence.
    pub exec_host: Option<&'a str>,
    /// What this identity has learned (PLAN 7.3, Phase 14), or `None` when it
    /// has learned nothing yet.
    pub memories: Option<&'a str>,
    /// The skill catalog (PLAN 7.3, Phase 13): one line per runbook this
    /// identity may run, and never a step of one. That is the shape of that
    /// phase — the catalog is standing context, because choosing a procedure
    /// has to be possible on any turn, and the body is loaded by `skill_run`
    /// into the turn that asked for it. `None` for an identity granted no
    /// skills, which is every identity from before it.
    pub skills: Option<&'a str>,
    /// The world's frame and status (PLAN 7.2), or `None` for a workspace with
    /// no constitution in it.
    ///
    /// The missed half of Phase 11, and the block that behaves least like the
    /// others: it is a *constraint* rather than a fact. A session opened on a
    /// world reads `world/`, does not write it, and does not reopen the sources
    /// it was perceived from — and that has to be a harness injection rather
    /// than a skill, because a skill can be skipped and forgetting this is the
    /// defect the whole shape exists to prevent (PLAN 7.2, *Frame, not a
    /// skill*). What it carries beyond the frame is *status* — which files the
    /// constitution holds, what it declares as its sources, whether any of them
    /// have visibly moved — and never `essence.md` itself.
    pub world: Option<&'a str>,
    /// The shared-workspace digest (PLAN 7.3, Phase 11), or `None` for a folder
    /// that does not use the convention.
    pub shared: Option<&'a str>,
    /// What this session's older turns folded into (PLAN 7.3, Phase 14), or
    /// `None` for a session that has never been compacted.
    pub compacted: Option<&'a str>,
    /// Whether a routine started this run, with nobody in front of it
    /// (PLAN 7.3, Phase 16).
    ///
    /// Said in the system message for the reason a missing workspace is: the
    /// model is better off being told than discovering it one refusal at a
    /// time. The routine's own opening message says it too, and this is what
    /// keeps it true in round five of a run whose first message has folded.
    pub unattended: bool,
}

/// The system message for a session: who it is, what it knows, where it is,
/// and what has already happened in it.
///
/// The order is the order of how slowly the parts change, so reading it top to
/// bottom is reading from "what is always true" to "what is true right now" —
/// which is also the order that survives a model skimming it.
///
/// 1. The standing instructions, which never change.
/// 2. The identity, which changes when someone edits it.
/// 3. The workspace, which changes when the project does.
/// 4. The memories, which change when this identity learns something.
/// 5. The skill catalog, which changes when somebody writes a runbook.
/// 6. The world, which changes when a human amends the constitution — and
///    which sits above the cabinet because it is what the cabinet is judged
///    against.
/// 7. The shared digest, which changes on every request.
/// 8. The folded state, which is this session's own history, and so sits
///    closest to the conversation it stands in for.
///
/// The workspace is named in full because a model asked to work "in the
/// project" with no path guesses one, and a guessed absolute path is exactly
/// the tool call the user then has to read carefully and refuse.
///
/// The prompt stays a policy summary plus what is true right now (PLAN 7.1,
/// *System prompt*). That is why an identity's instructions are capped in the
/// store, why a runbook is a file this message names rather than text it
/// carries, and why each of the five blocks appended at the end has a cap of
/// its own.
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

    match ctx.workspace {
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

    // Immediately after, because it qualifies what was just said: the folder
    // above is where the file tools work, and this is where a command over it
    // runs (PLAN 7.12). Absent for every project on this computer, which is
    // every project that has not been given a host — so a default install's
    // system message is byte for byte the one before this slice.
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
/// `history` is the messages that still reach the model, oldest first — the
/// whole transcript for a session that has never been compacted, and the raw
/// tail for one that has ([`compact::tail`](crate::compact::tail)). Slicing it
/// is the caller's, not this function's: what has folded is a fact about the
/// session, which lives in the store, and a projection that went looking for it
/// would need one.
///
/// Any `system` message in `history` is dropped: the one this function prepends
/// is the current one, and two system messages is not a shape the API defines.
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

    /// The five appended blocks go in one order, and the order is the point:
    /// slowest-changing first, and this session's own folded history last,
    /// where it sits against the conversation it stands in for. The world sits
    /// above the cabinet because it is what the cabinet is judged against.
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

    /// An identity that has learned nothing, in a session that has never been
    /// compacted, gets exactly the message Phase 13 produced. A phase that
    /// silently changed every existing session's prompt would be a migration
    /// nobody asked for.
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
