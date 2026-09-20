//! A provider with no model behind it.
//!
//! Lets the whole app — turn loop, events, approvals — run without an API key.
//!
//! * **Improvised** ([`FakeProvider::new`]): the reply is derived from the
//!   request (quotes the user, names the workspace, counts tools), so it also
//!   shows what [`transcript::build`](crate::agent::transcript::build) sent.
//! * **Scripted** ([`FakeProvider::scripted`]): exact event sequences per turn,
//!   for tests.
//!
//! Improvised mode only calls tools on explicit trigger words
//! ([`WRITE_TRIGGER`], [`RUN_TRIGGER`], [`CAPTURE_TRIGGER`], [`SKILL_TRIGGER`],
//! [`REMEMBER_TRIGGER`], [`DELEGATE_TRIGGER`]) — never on its own initiative.
//! A delegated run (offered `handoff_return`) and a scheduled run follow their
//! script. Tokens stream with a small delay; [`FakeProvider::instant`] removes
//! it for tests.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::agent::wire::{ModelEvent, ModelRequest, StopReason, Usage, WireMessage};

use super::{Provider, STREAM_BUFFER};

/// The model id this provider reports.
pub const FAKE_MODEL: &str = "aegis-fake-1";

/// Asks for an `fs_write` (Phase 6 walkthrough). Trigger words match
/// case-insensitively anywhere in the message.
pub const WRITE_TRIGGER: &str = "/write";

/// Asks for a `shell_exec` whose output streams (Phase 7 walkthrough).
pub const RUN_TRIGGER: &str = "/run";

/// Asks for a `screen_capture` (Phase 9 walkthrough).
pub const CAPTURE_TRIGGER: &str = "/capture";

/// Runs the first catalog skill: `skill_run`, then `skill_return`, without
/// carrying out the steps (Phase 13 walkthrough).
pub const SKILL_TRIGGER: &str = "/skill";

/// Asks for a `memory_write` (Phase 14 walkthrough).
pub const REMEMBER_TRIGGER: &str = "/remember";

/// Delegates two briefs in parallel and reads the board back (Phase 15
/// walkthrough). `/delegate Scribe` picks the owner; the default is the
/// built-in identity.
pub const DELEGATE_TRIGGER: &str = "/delegate";

/// What the triggered memory says: fixed, and about the demo, never invented
/// about the user.
pub const REMEMBER_TEXT: &str =
    "the scripted provider was asked to demonstrate how a memory is recorded";

/// The file the triggered write targets, relative to the workspace.
pub const WRITE_TARGET: &str = "aegis-approval-demo.txt";

/// Delay between tokens in the improvised reply: visible streaming, and time
/// for a human cancel to land mid-stream.
const TOKEN_DELAY: Duration = Duration::from_millis(18);

/// A provider that answers without a model.
#[derive(Debug)]
pub struct FakeProvider {
    model: String,
    delay: Duration,
    /// Remaining scripted turns. Empty means improvise.
    script: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProvider {
    /// The improvising provider the application runs with.
    pub fn new() -> Self {
        Self {
            model: FAKE_MODEL.to_owned(),
            delay: TOKEN_DELAY,
            script: Mutex::new(VecDeque::new()),
        }
    }

    /// [`FakeProvider::new`] with no pacing, for tests.
    pub fn instant() -> Self {
        Self {
            delay: Duration::ZERO,
            ..Self::new()
        }
    }

    /// A provider that replays `turns`, one sequence per request, then
    /// improvises.
    pub fn scripted(turns: Vec<Vec<ModelEvent>>) -> Self {
        Self {
            delay: Duration::ZERO,
            script: Mutex::new(turns.into()),
            ..Self::new()
        }
    }

    /// Takes the next scripted turn, if there is one.
    fn next_script(&self) -> Option<Vec<ModelEvent>> {
        self.script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
    }
}

impl Provider for FakeProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self, request: ModelRequest) -> mpsc::Receiver<ModelEvent> {
        let (tx, rx) = mpsc::channel(STREAM_BUFFER);

        let events = match self.next_script() {
            Some(scripted) => scripted,
            None => improvise(&request),
        };

        let delay = self.delay;

        tokio::spawn(async move {
            for event in events {
                // A closed channel means the turn stopped listening — it was
                // cancelled, or the window went away. Producing tokens nobody
                // will read is the one thing a cancelled provider must not do.
                if tx.send(event).await.is_err() {
                    tracing::debug!("the fake provider's listener went away");
                    return;
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
        });

        rx
    }
}

/// Builds a reply out of the request itself, so it reports what was actually
/// sent.
fn improvise(request: &ModelRequest) -> Vec<ModelEvent> {
    let said = last_user_text(request);
    let asked = said.to_lowercase();

    // Before `already_answered`: this trigger spans rounds and decides from
    // the tool results.
    if asked.contains(SKILL_TRIGGER) {
        return skill_turn(request);
    }

    // A scheduled run follows its runbook, whatever the message (Phase 16).
    if said.contains(crate::schedule::OPENING_MARKER) {
        return routine_turn(request, &said);
    }

    // A run a brief opened answers with a `handoff_return` and nothing else,
    // whatever was typed into it: the only thing the identity that briefed it
    // will ever see is that call (PLAN 7.3, Phase 15).
    if is_delegated(request) && !already_answered(request) {
        return return_the_brief(request);
    }

    // Also before `already_answered`, and for the same reason as the skill
    // trigger: its second round is the one where the board has come back.
    if asked.contains(DELEGATE_TRIGGER) {
        return delegate_turn(request, &said);
    }

    // The four things this provider will ask for, and only because the user
    // named them. Three reach the machine; the fourth reaches the identity's
    // own memory, which is why it is here rather than answered inline.
    if !already_answered(request) {
        if asked.contains(WRITE_TRIGGER) {
            return ask_to_write(&said);
        }
        if asked.contains(RUN_TRIGGER) {
            return ask_to_run();
        }
        if asked.contains(CAPTURE_TRIGGER) {
            return ask_to_capture();
        }
        if asked.contains(REMEMBER_TRIGGER) {
            return ask_to_remember();
        }
    }

    let workspace = workspace_line(request);
    let tools = request.tools.len();

    let reply = format!(
        "You said: \u{201c}{said}\u{201d}\n\n\
         There is no model behind this reply. It comes from the scripted \
         provider in `agent/provider/fake.rs`, streamed a token at a time so \
         the transcript, the session list and cancellation can all be \
         exercised end to end. Name a base URL and a model in Settings and a \
         real one answers instead; nothing above this line changes.\n\n\
         {workspace}\n\
         I was offered {tools} tool{plural}, and I will not call one unless \
         you ask. Send a message containing `{WRITE_TRIGGER}` and I will \
         request a file write; `{RUN_TRIGGER}` and I will request a command; \
         `{CAPTURE_TRIGGER}` and I will ask to photograph your primary \
         display. Any of them you can refuse, allow once, or allow for the \
         rest of the session, and find the audit line on disk afterwards. \
         `{SKILL_TRIGGER}` runs the first skill this identity was granted, \
         which takes two rounds and needs no approval at all. \
         `{REMEMBER_TRIGGER}` asks to remember something — the one call here \
         that touches nothing on your machine and still asks, because a memory \
         reaches the top of every later reply this identity gives.",
        plural = if tools == 1 { "" } else { "s" },
    );

    let mut events: Vec<ModelEvent> = tokens(&reply)
        .into_iter()
        .map(|text| ModelEvent::TextDelta { text })
        .collect();

    // Counted rather than invented: a usage figure that looked plausible would
    // be indistinguishable from a real one in the UI, and this one is honestly
    // a token count of a string nobody was charged for.
    let completion = u64::try_from(events.len()).unwrap_or(u64::MAX);
    let prompt = u64::try_from(request.messages.len()).unwrap_or(u64::MAX);

    events.push(ModelEvent::Finish {
        reason: StopReason::Stop,
        usage: Some(Usage {
            prompt_tokens: prompt,
            // The scripted provider has no cache and never pretends to.
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            completion_tokens: completion,
            total_tokens: prompt.saturating_add(completion),
        }),
    });
    events
}

/// A turn that asks to write a file, arguments in one fragment
/// ([`wire`](crate::agent::wire) tests fragmentation).
fn ask_to_write(said: &str) -> Vec<ModelEvent> {
    let content = format!(
        "Written by the fake provider in `agent/provider/fake.rs`, because a \
         message containing `{WRITE_TRIGGER}` asked it to.\n\nWhat was said: \
         {said}\n"
    );
    let arguments = serde_json::json!({
        "path": WRITE_TARGET,
        "content": content,
    })
    .to_string();

    vec![
        ModelEvent::TextDelta {
            text: format!("Writing `{WRITE_TARGET}` — this needs your approval.\n"),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::FS_WRITE.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// A turn that asks to list the workspace. On Windows it uses `powershell`
/// with explicit ANSI colours, so the demo shows how escapes are handled; the
/// script is one argument PowerShell parses itself.
fn ask_to_run() -> Vec<ModelEvent> {
    let (program, args, what): (&str, Vec<&str>, &str) = if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                r#"$e=[char]27; "${e}[1m$PWD${e}[0m"; Get-ChildItem | ForEach-Object { "  ${e}[36m$($_.Name)${e}[0m  $($_.Length)" }"#,
            ],
            "list the workspace in colour",
        )
    } else {
        ("ls", vec!["-la"], "list the workspace")
    };

    let arguments = serde_json::json!({
        "program": program,
        "args": args,
    })
    .to_string();

    vec![
        ModelEvent::TextDelta {
            text: format!(
                "Running `{program}` to {what} — this needs your approval. The dialog shows \
                 the exact arguments.\n"
            ),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::SHELL_EXEC.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// A turn that asks to capture the primary display, with no arguments.
fn ask_to_capture() -> Vec<ModelEvent> {
    vec![
        ModelEvent::TextDelta {
            text: "Capturing your primary display — this needs your approval, every time, \
                   and you will see the result. I will only be told where it was written.\n"
                .to_owned(),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::SCREEN_CAPTURE.to_owned()),
            args_delta: "{}".to_owned(),
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// A turn that asks to remember one thing — still approved, since a memory
/// shapes every later reply.
fn ask_to_remember() -> Vec<ModelEvent> {
    let arguments = serde_json::json!({
        "kind": "convention",
        "text": REMEMBER_TEXT,
        "source": "agent/provider/fake.rs",
    })
    .to_string();

    vec![
        ModelEvent::TextDelta {
            text: "Remembering one thing — this needs your approval. Nothing on your machine \
                   changes; what changes is what I carry into every later reply as this \
                   identity.\n"
                .to_owned(),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::MEMORY_WRITE.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// A turn that runs a skill: `skill_run`, then `skill_return`, then a word,
/// chosen from this turn's tool results. The steps are not carried out.
fn skill_turn(request: &ModelRequest) -> Vec<ModelEvent> {
    let answers = answers_this_turn(request);

    if answers
        .iter()
        .any(|body| body.contains(r#""tool":"skill_return""#))
    {
        return say(
            "That is the whole loop: the catalog named the runbook, `skill_run` handed me its \
             steps for this turn only, and `skill_return` checked the status object against the \
             shape a handoff has to come back in. Both calls are on the audit log with the \
             skill's name on them. A real model would have carried the steps out in between; \
             there is none behind this reply.",
        );
    }

    match answers
        .iter()
        .rev()
        .find(|body| body.contains(r#""tool":"skill_run""#))
    {
        // The run is open. Close it, and deliberately with a `blocked`: this
        // provider did not do the work, and a `done` claiming otherwise is the
        // exact thing the return validation exists to refuse.
        Some(body) if body.contains(r#""ok":true"#) => match meta_skill(body) {
            Some(name) => ask_to_return(&name),
            None => say("The runbook loaded but its envelope named no skill, which is a bug."),
        },
        Some(_) => say(
            "The runbook did not load — the result above says why. Nothing was run, and there is \
             nothing to return.",
        ),
        None => match first_catalog_skill(request) {
            Some(name) => ask_to_load(&name),
            None => say(
                "This identity was granted no skills, so there is no catalog in my instructions \
                 and nothing to run. Skills are granted per identity in Settings → Identities, \
                 from the runbooks in your library and in this workspace's `skills/` folder.",
            ),
        },
    }
}

/// A scheduled run in three rounds: load the named runbook, write into
/// `.aegis/status/` (allowed only if the routine was signed for it), return.
fn routine_turn(request: &ModelRequest, said: &str) -> Vec<ModelEvent> {
    let answers = answers_this_turn(request);

    let Some(name) = skill_named(said) else {
        return say(
            "The opening message named no runbook, which is a bug in the scheduler rather than \
             in this reply.",
        );
    };

    // Third round: the write has come back, one way or the other. Either way
    // the run owes a return, and it says which happened.
    if let Some(body) = answers
        .iter()
        .rev()
        .find(|body| body.contains(r#""tool":"fs_write""#))
    {
        return close_the_run(&name, body.contains(r#""ok":true"#));
    }

    // Second round: the runbook loaded, so there is something to do.
    match answers
        .iter()
        .rev()
        .find(|body| body.contains(r#""tool":"skill_run""#))
    {
        Some(body) if body.contains(r#""ok":true"#) => write_the_status(&name),
        Some(_) => close_the_run(&name, false),
        None => ask_to_load(&name),
    }
}

/// The write a watch routine exists to make.
fn write_the_status(name: &str) -> Vec<ModelEvent> {
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let arguments = serde_json::json!({
        "path": format!("{}/status/{name}.md", crate::workspace::CABINET_DIR),
        "content": format!(
            "# {name}

Last scheduled run: {stamp}

There is no model behind this run —              the scripted provider wrote this line to show that a routine can reach the disk              while the window is shut, and only as far as it was signed for.
"
        ),
        "create_dirs": true,
    })
    .to_string();

    vec![
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::FS_WRITE.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// The return a scheduled run finishes with, whichever way the write went.
fn close_the_run(name: &str, wrote: bool) -> Vec<ModelEvent> {
    let arguments = if wrote {
        serde_json::json!({
            "status": "done",
            "summary": format!("Ran {name} on its schedule and wrote .aegis/status/{name}.md."),
            "artefacts": [format!("{}/status/{name}.md", crate::workspace::CABINET_DIR)],
        })
    } else {
        serde_json::json!({
            "status": "blocked",
            "summary": format!(
                "Ran {name} on its schedule. The status file could not be written — nobody is \
                 watching this run, so the write was refused rather than put to anyone."
            ),
            "open_questions": ["Should this routine be signed for writes inside the workspace?"],
        })
    }
    .to_string();

    vec![
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::SKILL_RETURN.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// The runbook a scheduled run's opening message names.
fn skill_named(said: &str) -> Option<String> {
    let at = said.find("skill:")? + "skill:".len();
    let rest = &said[at..];
    let end = rest.find('`')?;
    let name = rest[..end].trim();

    (!name.is_empty()).then(|| name.to_owned())
}

/// A turn that loads one runbook, and nothing else.
fn ask_to_load(name: &str) -> Vec<ModelEvent> {
    let arguments = serde_json::json!({ "name": name }).to_string();

    vec![
        ModelEvent::TextDelta {
            text: format!(
                "Loading the `{name}` runbook. This one needs no approval — it reads a file you \
                 put in your own library or workspace, and everything it then tells me to do is \
                 gated exactly as it would be otherwise.\n"
            ),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::SKILL_RUN.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// A turn that closes the run with an honest `blocked`.
fn ask_to_return(name: &str) -> Vec<ModelEvent> {
    let arguments = serde_json::json!({
        "status": "blocked",
        "summary": format!(
            "Loaded the {name} runbook and read its steps. There is no model behind this \
             provider, so none of them were carried out."
        ),
        "open_questions": [
            "Name a base URL and a model in Settings, and a real one will follow these steps."
        ],
        "next_owner": "human",
    })
    .to_string();

    vec![
        ModelEvent::TextDelta {
            text: "I have the steps. Returning `blocked` rather than `done`, because I did not \
                   do them — a `done` here would be refused anyway: the runner checks that the \
                   artefacts a run claims are really on disk.\n"
                .to_owned(),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::SKILL_RETURN.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// A turn that hands out two briefs (to show fan-out), waits, and reads the
/// board back. The owner is the one named after the trigger, or the built-in
/// identity.
fn delegate_turn(request: &ModelRequest, said: &str) -> Vec<ModelEvent> {
    let answers = answers_this_turn(request);

    if let Some(board) = answers
        .iter()
        .find(|body| body.contains(r#""tool":"handoff_delegate""#))
    {
        return say(if board.contains(r#""ok":true"#) {
            "Both briefs came back. What I was handed is a board — a status, an artefact list \
             and any open questions per owner — and not one line of what either of them \
             actually said to itself. Their sessions are in the sidebar if you want to read \
             them; the point is that I did not have to. Each of them ran under its own \
             identity's allow-list, so anything they wanted to write asked you, not me."
        } else {
            "The delegation did not go out. The refusal is on the tool card above and on the \
             audit log; nothing was started, so there is nothing to stop."
        });
    }

    let owner = owner_named(said);
    let brief = |goal: &str, done: &str| {
        serde_json::json!({
            "goal": goal,
            "owner": owner,
            "priority": "normal",
            "inputs": [],
            "constraints": ["say what you are, and do not touch anything"],
            "definition_of_done": done,
            "approval_needed": "nothing: neither brief asks for a tool",
            "return_format": "status",
        })
    };

    let arguments = serde_json::json!({
        "briefs": [
            brief(
                "Say, in one line, which identity you are running as",
                "the return names the identity",
            ),
            brief(
                "Say, in one line, what a brief gave you that a chat would not",
                "the return answers the question",
            ),
        ],
    })
    .to_string();

    vec![
        ModelEvent::TextDelta {
            text: format!(
                "Handing two briefs to `{owner}`. They run at the same time, in sessions of \
                 their own, and I will see what they return rather than what they said.\n"
            ),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::HANDOFF_DELEGATE.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// The identity named after the trigger (`/delegate Scribe`), or the built-in
/// one.
fn owner_named(said: &str) -> String {
    // Search and slice the lowered copy: lowering can change byte offsets
    // (`İ`), and owner names match case-insensitively anyway.
    let lower = said.to_lowercase();
    let Some(at) = lower.find(DELEGATE_TRIGGER) else {
        return crate::store::Agent::builtin().name;
    };

    match lower[at + DELEGATE_TRIGGER.len()..]
        .split_whitespace()
        .next()
    {
        Some(name) => name.to_owned(),
        None => crate::store::Agent::builtin().name,
    }
}

/// The answer a delegated run gives: a `handoff_return`, never prose.
fn return_the_brief(request: &ModelRequest) -> Vec<ModelEvent> {
    let goal = last_user_text(request)
        .lines()
        .find_map(|line| line.strip_prefix("goal: ").map(str::to_owned))
        .unwrap_or_else(|| "the brief".to_owned());

    let identity = request
        .messages
        .iter()
        .find_map(|message| match message {
            WireMessage::System { content } => content
                .lines()
                .find_map(|line| line.strip_prefix("You are working as `"))
                .and_then(|line| line.split('`').next())
                .map(str::to_owned),
            _ => None,
        })
        .unwrap_or_else(|| crate::store::Agent::builtin().name);

    let arguments = serde_json::json!({
        "status": "blocked",
        "summary": format!(
            "Read the brief as `{identity}`. There is no model behind this provider, so \
             \u{201c}{goal}\u{201d} was not carried out."
        ),
        "evidence": ["the brief arrived as this run's first message"],
        "open_questions": [
            "Name a base URL and a model in Settings, and a real one will answer this brief."
        ],
        "next_owner": "human",
    })
    .to_string();

    vec![
        ModelEvent::TextDelta {
            text: "Returning `blocked`: I read the brief and did not do it. A `done` would be \
                   refused anyway — the runner checks the artefacts a return claims.\n"
                .to_owned(),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call_{}", uuid::Uuid::new_v4())),
            name: Some(crate::policy::tool::HANDOFF_RETURN.to_owned()),
            args_delta: arguments,
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// Whether this request is a delegated run's: offered `handoff_return` and not
/// `handoff_delegate` (`Turn::held` never offers both).
fn is_delegated(request: &ModelRequest) -> bool {
    offers(request, crate::policy::tool::HANDOFF_RETURN)
        && !offers(request, crate::policy::tool::HANDOFF_DELEGATE)
}

/// Whether one tool is on the request's list.
fn offers(request: &ModelRequest, name: &str) -> bool {
    request.tools.iter().any(|tool| {
        tool.get("function")
            .and_then(|function| function.get("name"))
            .and_then(serde_json::Value::as_str)
            == Some(name)
    })
}

/// A plain streamed reply, with no tool call in it.
fn say(text: &str) -> Vec<ModelEvent> {
    let mut events: Vec<ModelEvent> = tokens(text)
        .into_iter()
        .map(|text| ModelEvent::TextDelta { text })
        .collect();

    events.push(ModelEvent::Finish {
        reason: StopReason::Stop,
        usage: None,
    });
    events
}

/// The `tool` messages after the most recent user message, oldest first.
fn answers_this_turn(request: &ModelRequest) -> Vec<&str> {
    let from = request
        .messages
        .iter()
        .rposition(|message| matches!(message, WireMessage::User { .. }))
        .unwrap_or(0);

    request.messages[from..]
        .iter()
        .filter_map(|message| match message {
            WireMessage::Tool { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect()
}

/// The skill an envelope's `meta` names
/// ([`META_SKILL`](crate::skills::META_SKILL)).
fn meta_skill(envelope: &str) -> Option<String> {
    let key = format!("\"{}\":\"", crate::skills::META_SKILL);
    let at = envelope.find(&key)? + key.len();
    let rest = &envelope[at..];
    let end = rest.find('"')?;

    Some(rest[..end].to_owned())
}

/// The first skill the system message's catalog offers, if any.
fn first_catalog_skill(request: &ModelRequest) -> Option<String> {
    let system = request.messages.iter().find_map(|message| match message {
        WireMessage::System { content } => Some(content.as_str()),
        _ => None,
    })?;

    system.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("- `")?;
        let end = rest.find('`')?;
        Some(rest[..end].to_owned())
    })
}

/// Whether this turn's call (after the most recent user message) was already
/// answered.
fn already_answered(request: &ModelRequest) -> bool {
    let Some(latest) = request
        .messages
        .iter()
        .rposition(|message| matches!(message, WireMessage::User { .. }))
    else {
        return false;
    };

    request.messages[latest..]
        .iter()
        .any(|message| matches!(message, WireMessage::Tool { .. }))
}

/// The last thing the user said, or a stand-in when they said nothing.
fn last_user_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            WireMessage::User { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "(nothing)".to_owned())
}

/// One line reporting the workspace the built system message named.
fn workspace_line(request: &ModelRequest) -> String {
    let system = request.messages.iter().find_map(|message| match message {
        WireMessage::System { content } => Some(content.as_str()),
        _ => None,
    });

    match system {
        Some(content) => match content
            .lines()
            .find_map(|line| line.strip_prefix("The workspace is: "))
        {
            Some(path) => format!("The workspace I was given is {path}."),
            None => "I was given no workspace, so no tool could run anyway.".to_owned(),
        },
        None => "I was sent no system message at all, which is a bug.".to_owned(),
    }
}

/// Splits text into streaming tokens that concatenate back to the input.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_gap = false;

    for ch in text.chars() {
        let is_space = ch.is_whitespace();

        // A word boundary is the first non-space after a run of spaces.
        if !is_space && in_gap && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        current.push(ch);
        in_gap = is_space;
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests;
