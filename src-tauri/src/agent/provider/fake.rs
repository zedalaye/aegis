//! A provider with no model behind it.
//!
//! It exists so everything above it can be finished and exercised before there
//! is an API key anywhere near the project: the turn loop, the transcript, the
//! event plumbing, the session list, cancellation, and — from Phase 6 — the
//! approval gate. All of that is provider-shaped work, and none of it should
//! wait on a network client.
//!
//! Two modes:
//!
//! * **Improvised** ([`FakeProvider::new`]) — the reply is derived from the
//!   request. It quotes the user, names the workspace it was given and counts
//!   the tools it was offered, so a reply that streams into the window is
//!   evidence that [`transcript::build`](crate::agent::transcript::build)
//!   really did carry those things.
//! * **Scripted** ([`FakeProvider::scripted`]) — exact event sequences, one
//!   per turn, consumed in order. This is how a test drives a tool call, a
//!   truncated arguments string or a provider error through the loop.
//!
//! The improvised mode has six deliberate exceptions to "never touch the
//! machine", and they are what make the approval gate — and, from Phase 13,
//! the skill runner — usable before there is a model: a message containing
//! [`WRITE_TRIGGER`] makes it ask for an `fs_write` (PLAN 6, Phase 6 — "the
//! fake provider is scripted to request an `fs_write`"), one containing
//! [`RUN_TRIGGER`] makes it ask for a `shell_exec` (Phase 7), one containing
//! [`CAPTURE_TRIGGER`] makes it ask for a `screen_capture` (Phase 9), and one
//! containing [`SKILL_TRIGGER`] makes it load and close a skill run (Phase
//! 13), one containing [`REMEMBER_TRIGGER`] makes it ask to remember something
//! (Phase 14), and one containing [`DELEGATE_TRIGGER`] makes it hand two briefs
//! to another identity and wait for the board (Phase 15). All six are words the
//! user has to type, not heuristics over what they said: a fake model that
//! decided on its own when to reach for the disk, for a process, for the
//! screen, for its own memory or for somebody else's turn would be exactly the
//! behaviour the gate exists to catch.
//!
//! There is one exception, and it is not a decision the provider makes: a
//! request that offers `handoff_return` is a run a brief opened, and it is
//! answered with that call. The only thing the identity that briefed it will
//! ever see is the report, so a fake that replied with prose would be
//! demonstrating a failure rather than the loop.
//!
//! Tokens are emitted with a small delay so streaming is visibly streaming and
//! a cancel has something to interrupt. Tests use [`FakeProvider::instant`],
//! which sets the delay to zero.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::agent::wire::{ModelEvent, ModelRequest, StopReason, Usage, WireMessage};

use super::{Provider, STREAM_BUFFER};

/// The model id this provider reports.
pub const FAKE_MODEL: &str = "aegis-fake-1";

/// The word that makes the improvising provider ask for a file write.
///
/// Typing it is the Phase 6 walkthrough: the model asks, the approval dialog
/// opens, and allow-once / allow-session / deny can each be seen to do what
/// they say. Matched case-insensitively anywhere in the user's message.
pub const WRITE_TRIGGER: &str = "/write";

/// The word that makes the improvising provider ask to run a command.
///
/// Typing it is the Phase 7 walkthrough: the dialog names the exact program,
/// arguments and working directory, and once allowed the output streams into
/// the transcript as the command produces it. Matched case-insensitively
/// anywhere in the user's message.
pub const RUN_TRIGGER: &str = "/run";

/// The word that makes the improvising provider ask to capture the screen.
///
/// Typing it is the Phase 9 walkthrough: the dialog names the display and both
/// of its sizes, and once allowed the capture appears in the transcript as a
/// thumbnail while the model is told only where the file is. Matched
/// case-insensitively anywhere in the user's message.
pub const CAPTURE_TRIGGER: &str = "/capture";

/// The word that makes the improvising provider run a skill.
///
/// Typing it is the Phase 13 walkthrough, and it is the one trigger that takes
/// two rounds: the provider calls `skill_run` on the first runbook the catalog
/// offers, reads the steps back, and then closes the run with a `skill_return`
/// — so the catalog, the loaded body, the validated return and the skill name
/// on the audit lines can all be seen without a model. It stops there rather
/// than carrying the steps out, because a scripted provider following a
/// runbook would be pretending to be the thing this trigger exists to make
/// visible. Matched case-insensitively anywhere in the user's message.
pub const SKILL_TRIGGER: &str = "/skill";

/// The word that makes the improvising provider ask to remember something.
///
/// Typing it is the Phase 14 walkthrough: the dialog shows the exact sentence
/// that would be remembered, and once allowed it appears under Memory in
/// Settings and at the top of the *next* reply this identity gives. Matched
/// case-insensitively anywhere in the user's message.
pub const REMEMBER_TRIGGER: &str = "/remember";

/// The word that makes the improvising provider hand work to other identities.
///
/// Typing it is the Phase 15 walkthrough, and it is the only trigger that
/// starts *other agents*: the dialog names the owners, two sessions open under
/// those identities and run at the same time, and what comes back into this
/// transcript is a board of statuses rather than either of their conversations.
/// The word may be followed by an identity's name — `/delegate Scribe` — and
/// with nothing after it the briefs go to the built-in assistant, which is the
/// one identity that certainly exists. Matched case-insensitively anywhere in
/// the user's message.
pub const DELEGATE_TRIGGER: &str = "/delegate";

/// What the triggered memory says.
///
/// Fixed, and about the demo rather than about the user's work. A scripted
/// provider that improvised a preference about someone it has never met would
/// be writing words they never said into the one store that outlives every
/// session — and unlike the demo file, a memory is not something you notice by
/// looking at your workspace.
pub const REMEMBER_TEXT: &str =
    "the scripted provider was asked to demonstrate how a memory is recorded";

/// The file the triggered write targets, relative to the workspace.
///
/// Inside the workspace and named after what it is, so the prompt a user reads
/// is about a file they would not mind existing. The write is still a write:
/// it goes through the same policy row, the same dialog and the same audit
/// line as any other.
pub const WRITE_TARGET: &str = "aegis-approval-demo.txt";

/// Delay between tokens in the improvised reply.
///
/// Fast enough not to be a wait, slow enough that a person can see text
/// arriving rather than appearing — and long enough that a cancel sent by a
/// human lands mid-stream, which is the thing being exercised.
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

    /// A provider that replays `turns`, one sequence per request.
    ///
    /// Once the script runs out it improvises, so a test that scripts a tool
    /// call does not also have to script the reply that follows the tool
    /// result.
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

/// Builds a reply out of the request itself.
///
/// Everything it says is measured from the request, so the text doubles as a
/// report on what the transcript actually sent: if the workspace line is wrong
/// or the tool count is zero, that is a real bug, visible in the window without
/// a debugger.
fn improvise(request: &ModelRequest) -> Vec<ModelEvent> {
    let said = last_user_text(request);
    let asked = said.to_lowercase();

    // Before `already_answered`, because this is the one trigger that spans
    // two rounds: its second round is exactly the round in which a tool
    // message exists. It decides what to do from what came back, which is why
    // it can be re-entered without asking for the same thing again.
    if asked.contains(SKILL_TRIGGER) {
        return skill_turn(request);
    }

    // A run a routine fired follows its runbook and returns, whatever is in
    // the message: it is the only kind of turn here that nobody typed into,
    // and the opening it was given is the whole of what it was asked for
    // (PLAN 7.3, Phase 16).
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

/// A turn that asks to write a file, and nothing else.
///
/// The arguments are streamed as one fragment rather than assembled from
/// several, because what is being exercised downstream is the approval gate,
/// not the tool-call assembler — [`wire`](crate::agent::wire) has its own
/// tests for the fragmented case.
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

/// A turn that asks to list the workspace, and nothing else.
///
/// The command is chosen for three properties and no others: it exists on
/// every platform the MVP targets, it prints enough to watch arriving, and on
/// Windows it prints it in colour — which is the interesting case, because
/// ANSI escape sequences are the other thing a pipe carries besides text, and
/// the point of a demo is to make what the runtime does with them visible
/// rather than to arrange for it never to come up.
///
/// `powershell` rather than `cmd`: it is what a Windows user actually works
/// in, and Windows PowerShell 5.1 does not colour a redirected `Get-ChildItem`
/// on its own, so the escapes are written explicitly. Note that the script is
/// one argument, which PowerShell then parses itself — that is PowerShell's
/// doing and it is visible in the approval dialog. `shell_exec` still passes
/// an argument vector and still puts no shell of its own in the way.
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

/// A turn that asks to capture the screen, and nothing else.
///
/// No arguments at all: `display` defaults to the primary one, which is the
/// only display this build captures, and a demo that spelled it out would be
/// demonstrating a field rather than the gate. What is worth watching here is
/// the asymmetry the tool is built around — the person sees the picture in the
/// transcript, and the model is told only that a file exists and how big it is.
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

/// A turn that asks to remember one thing, and nothing else.
///
/// The one trigger whose call touches nothing on the machine and is still put
/// to the user. That is the point of it: a memory reaches the top of every
/// later reply this identity gives, which makes it closer to an instruction
/// than to a note, and the dialog shows the whole sentence because a memory is
/// one sentence.
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

/// A turn that runs a skill: `skill_run`, then `skill_return`, then a word.
///
/// Which of the three depends only on what came back, so the same function
/// answers every round of the demo. That is also what makes it honest about
/// the phase: a run is a span inside one turn, and the provider can see where
/// in the span it is by reading the tool messages of that turn.
///
/// It deliberately does not carry the runbook's steps out. A scripted provider
/// following a procedure would be imitating the model, and what this trigger
/// exists to show is the runner — the catalog, the body arriving on demand,
/// the return being validated, and the skill's name on the audit lines.
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

/// The three rounds a scheduled run takes without a model behind it.
///
/// Load the runbook the routine named, write a line into `.aegis/status/`, close with
/// a return. It is the one script here that carries a job *out* rather than
/// stopping at the gate, and that is deliberate: what the Phase 16 walkthrough
/// has to show is a run happening with the window shut — the write going
/// through because a person signed the routine for it, or being refused because
/// they did not, with nobody asked either way.
fn routine_turn(request: &ModelRequest, said: &str) -> Vec<ModelEvent> {
    let answers = answers_this_turn(request);

    let Some(name) = skill_named(said) else {
        return say(
            "The opening message named no runbook, which is a bug in the scheduler rather than              in this reply.",
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
                "Ran {name} on its schedule. The status file could not be written — nobody is                  watching this run, so the write was refused rather than put to anyone."
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
///
/// Read out of the message rather than guessed from the catalog, because the
/// routine named one: a scheduled run that ran whatever happened to be first in
/// the list would be demonstrating the wrong thing entirely.
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

/// A turn that hands two briefs out, waits, and reads the board back.
///
/// The Phase 15 walkthrough, and the one trigger that starts other agents: the
/// dialog names the owners, two sessions open under those identities, each runs
/// its own turn through the same gate, and what comes back here is a board of
/// statuses rather than either of their conversations.
///
/// Two briefs rather than one, because the claim being demonstrated is that
/// they run *at the same time* — one brief would prove a call, not a fan-out.
/// Both go to the identity the user named after the trigger, or to the built-in
/// one, since this provider cannot see the registry and inventing an owner
/// would be a delegation that fails before it starts.
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

/// The identity named after the trigger, or the built-in one.
///
/// `/delegate Scribe` hands the briefs to Scribe. With nothing after it they go
/// to the assistant, which is the one identity that certainly exists.
fn owner_named(said: &str) -> String {
    // The lowered copy is both searched *and* sliced. Lowering can change a
    // string's length — `İ` is one character and two lower-case ones — so an
    // index found in the copy is not necessarily a character boundary in the
    // original, and slicing the original with it would panic on a message that
    // happens to contain one. What comes back is lower case, which costs
    // nothing: the runner matches an owner's name case-insensitively.
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

/// The answer a delegated run gives: a `handoff_return`, and nothing else.
///
/// A specialist that ended its turn with prose would have said nothing anybody
/// is listening for — which is exactly the failure the bus turns into a second
/// attempt and then an escalation, so this provider does the thing a real model
/// is supposed to do rather than demonstrating the failure.
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

/// Whether this request is a delegated run's.
///
/// Read off the tools rather than off the text, because there it is a fact
/// rather than a guess about a message. `Turn::held` offers `handoff_return`
/// to a run a brief opened and takes `handoff_delegate` away from it, and does
/// the mirror of that everywhere else — so the two are never on one list, and
/// "return without delegate" is the state itself.
///
/// Both halves are checked rather than only the first, so a caller that hands
/// this provider the whole registry (which no turn does) reads as the ordinary
/// session it is rather than as a run with a brief behind it.
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

/// The `tool` messages that belong to this turn, oldest first.
///
/// Scoped after the most recent user message, for the reason
/// [`already_answered`] is: "this turn" and "this conversation" are different
/// questions, and only the first one says where in a skill run we are.
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

/// The skill an envelope's `meta` names.
///
/// Read out of the JSON as text rather than parsed: this provider has no
/// business owning a copy of the envelope's shape, and the one field it needs
/// is the one the runner documents as the run's name
/// ([`META_SKILL`](crate::skills::META_SKILL)).
fn meta_skill(envelope: &str) -> Option<String> {
    let key = format!("\"{}\":\"", crate::skills::META_SKILL);
    let at = envelope.find(&key)? + key.len();
    let rest = &envelope[at..];
    let end = rest.find('"')?;

    Some(rest[..end].to_owned())
}

/// The first skill the catalog in the system message offers.
///
/// Read back out of the built request, like [`workspace_line`], so what this
/// picks is what the runtime actually told the model it could run — including
/// nothing, when the identity was granted none.
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

/// Whether the call this turn asked for has already been answered.
///
/// Scoped to the messages after the most recent user message, which is what
/// makes it "this turn" rather than "this conversation". Without the scope the
/// second round of a turn would ask again and burn all eight rounds on one
/// file; with the wrong scope — anywhere in the transcript — a session that
/// ever ran a tool could never trigger a write again.
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
            WireMessage::User { content } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "(nothing)".to_owned())
}

/// One line reporting the workspace the system message named.
///
/// Read back out of the built request rather than passed in separately, so it
/// reflects what was actually sent.
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

/// Splits text into streaming tokens.
///
/// Whitespace stays attached to the word before it, so concatenating every
/// token reproduces the input exactly — the property the transcript depends on
/// when it replaces the streamed buffer with the finalized message.
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
mod tests {
    use super::*;

    use crate::agent::transcript;
    use crate::agent::wire::WireMessage;

    use std::path::PathBuf;

    /// Collects a whole stream.
    async fn drain(provider: &FakeProvider, request: ModelRequest) -> Vec<ModelEvent> {
        let mut rx = provider.stream(request);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    /// The text of every `TextDelta`, concatenated.
    fn text_of(events: &[ModelEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The owner named after the trigger, and the built-in identity without
    /// one. Lowering a message can change its length, so the index the trigger
    /// is found at is only ever used on the string it was found in.
    #[test]
    fn the_delegate_trigger_names_an_owner_without_ever_slicing_mid_character() {
        let builtin = crate::store::Agent::builtin().name;

        assert_eq!(owner_named("/delegate Scribe please"), "scribe");
        assert_eq!(owner_named("please /DELEGATE Reader"), "reader");
        assert_eq!(owner_named("/delegate"), builtin);
        assert_eq!(owner_named("nothing here"), builtin);
        // `İ` lowers to two characters, so an index into the lowered copy is
        // past the end of the original by the time the trigger is reached.
        assert_eq!(owner_named("İİİ /delegate Scribe"), "scribe");
    }

    fn request_saying(text: &str) -> ModelRequest {
        let agent = crate::store::Agent::builtin();
        let root = PathBuf::from("/home/p/work");

        transcript::build(
            FAKE_MODEL,
            &transcript::Context {
                agent: &agent,
                workspace: Some(&root),
                exec_host: None,
                memories: None,
                skills: None,
                world: None,
                shared: None,
                compacted: None,
                unattended: false,
            },
            &[crate::store::Message::user(text)],
            crate::tools::schemas(),
        )
    }

    #[tokio::test]
    async fn a_reply_streams_and_then_finishes() {
        let events = drain(&FakeProvider::instant(), request_saying("hello there")).await;

        assert!(events.len() > 5, "the reply is streamed, not sent whole");
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: StopReason::Stop,
                usage: Some(_)
            })
        ));
    }

    /// The reply is evidence about the request: if the transcript stopped
    /// carrying the workspace or the tool schemas, the text says so.
    #[tokio::test]
    async fn the_reply_reports_what_the_request_carried() {
        let events = drain(&FakeProvider::instant(), request_saying("hello there")).await;
        let text = text_of(&events);

        assert!(text.contains("hello there"), "{text}");
        assert!(text.contains("/home/p/work"), "{text}");
        assert!(
            text.contains(&format!("{} tools", crate::tools::schemas().len())),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_request_without_a_workspace_is_reported_as_such() {
        let agent = crate::store::Agent::builtin();
        let request = transcript::build(
            FAKE_MODEL,
            &transcript::Context {
                agent: &agent,
                workspace: None,
                exec_host: None,
                memories: None,
                skills: None,
                world: None,
                shared: None,
                compacted: None,
                unattended: false,
            },
            &[],
            Vec::new(),
        );
        let text = text_of(&drain(&FakeProvider::instant(), request).await);

        assert!(text.contains("no workspace"), "{text}");
    }

    #[tokio::test]
    async fn a_script_is_replayed_verbatim_and_then_exhausts() {
        let scripted = vec![vec![
            ModelEvent::TextDelta {
                text: "one".to_owned(),
            },
            ModelEvent::Finish {
                reason: StopReason::ToolCalls,
                usage: None,
            },
        ]];
        let provider = FakeProvider::scripted(scripted.clone());

        assert_eq!(
            drain(&provider, request_saying("go")).await,
            scripted[0],
            "the first turn is exactly the script"
        );

        // The second turn has no script left, so it improvises rather than
        // returning nothing.
        let second = drain(&provider, request_saying("and again")).await;
        assert!(second.len() > 1);
        assert!(text_of(&second).contains("and again"));
    }

    /// Concatenating the tokens must reproduce the text exactly: the UI
    /// appends deltas to a buffer and then swaps in the finalized message, and
    /// the two have to agree.
    #[test]
    fn tokens_reassemble_into_the_original_text() {
        for text in [
            "hello there friend",
            "  leading and trailing  ",
            "line one\n\nline two",
            "one",
            "",
            "\u{201c}quoted\u{201d} and punctuated.",
        ] {
            assert_eq!(
                tokens(text).concat(),
                text,
                "round trip failed for {text:?}"
            );
        }
    }

    #[test]
    fn tokens_break_on_words_rather_than_characters() {
        assert_eq!(tokens("a bc  d"), vec!["a ", "bc  ", "d"]);
    }

    /// A dropped receiver is how the turn loop says it stopped listening. The
    /// provider must not keep producing into a channel nobody reads.
    #[tokio::test]
    async fn a_dropped_listener_stops_the_stream() {
        let provider = FakeProvider::new();
        let rx = provider.stream(request_saying("a long enough message to stream"));
        drop(rx);

        // Nothing to assert but the absence of a panic: the send fails, the
        // task returns. Yielding gives it the chance to do so under the test
        // runtime.
        tokio::task::yield_now().await;
    }

    /// PLAN 6, Phase 6: the fake provider asks for an `fs_write` on demand, so
    /// the approval gate can be walked through without a model.
    #[tokio::test]
    async fn the_write_trigger_produces_a_real_fs_write_call() {
        let asked = drain(
            &FakeProvider::instant(),
            request_saying("please /write something"),
        )
        .await;

        let call = asked
            .iter()
            .find_map(|event| match event {
                ModelEvent::ToolCallDelta {
                    name, args_delta, ..
                } => Some((name.clone(), args_delta.clone())),
                _ => None,
            })
            .expect("a tool call");

        assert_eq!(call.0.as_deref(), Some(crate::policy::tool::FS_WRITE));

        // The arguments have to be one valid JSON object, or the turn answers
        // the call with a parse error instead of asking anyone about it.
        let args: serde_json::Value = serde_json::from_str(&call.1).expect("valid arguments");
        assert_eq!(args["path"], WRITE_TARGET);
        assert!(args["content"].as_str().is_some_and(|c| !c.is_empty()));

        assert!(matches!(
            asked.last(),
            Some(ModelEvent::Finish {
                reason: StopReason::ToolCalls,
                ..
            })
        ));
    }

    /// PLAN 6, Phase 7: the fake provider asks for a `shell_exec` on demand,
    /// so the shell tool can be walked through without a model.
    #[tokio::test]
    async fn the_run_trigger_produces_a_real_shell_exec_call() {
        let asked = drain(
            &FakeProvider::instant(),
            request_saying("please /run something"),
        )
        .await;

        let call = asked
            .iter()
            .find_map(|event| match event {
                ModelEvent::ToolCallDelta {
                    name, args_delta, ..
                } => Some((name.clone(), args_delta.clone())),
                _ => None,
            })
            .expect("a tool call");

        assert_eq!(call.0.as_deref(), Some(crate::policy::tool::SHELL_EXEC));

        let args: serde_json::Value = serde_json::from_str(&call.1).expect("valid arguments");
        let program = args["program"].as_str().expect("a program");
        assert!(!program.is_empty());
        assert!(args["args"].is_array());

        // The arguments have to be a vector, not a command line: a single
        // element carrying spaces would be a program name with spaces in it,
        // and it would not resolve.
        for argument in args["args"].as_array().expect("an array") {
            assert!(argument.is_string());
        }
    }

    /// PLAN 6, Phase 9: the fake provider asks for a `screen_capture` on
    /// demand, so the capture gate can be walked through without a model — and
    /// without a capture ever reaching a provider, since there is none.
    #[tokio::test]
    async fn the_capture_trigger_produces_a_real_screen_capture_call() {
        let asked = drain(
            &FakeProvider::instant(),
            request_saying("please /capture the screen"),
        )
        .await;

        let call = asked
            .iter()
            .find_map(|event| match event {
                ModelEvent::ToolCallDelta {
                    name, args_delta, ..
                } => Some((name.clone(), args_delta.clone())),
                _ => None,
            })
            .expect("a tool call");

        assert_eq!(call.0.as_deref(), Some(crate::policy::tool::SCREEN_CAPTURE));

        // An empty object, not an absent one: the turn parses the accumulated
        // arguments as JSON before anything is decided, and a call whose
        // arguments do not parse is answered rather than asked about.
        let args: serde_json::Value = serde_json::from_str(&call.1).expect("valid arguments");
        assert_eq!(args, serde_json::json!({}));
    }

    /// PLAN 7.3, Phase 14: the fake provider asks to remember something on
    /// demand, so the one gate that protects nothing on the machine can still
    /// be walked through without a model.
    #[tokio::test]
    async fn the_remember_trigger_asks_to_write_a_memory_about_the_demo() {
        let asked = drain(
            &FakeProvider::instant(),
            request_saying("please /remember this"),
        )
        .await;

        let call = asked
            .iter()
            .find_map(|event| match event {
                ModelEvent::ToolCallDelta {
                    name, args_delta, ..
                } => Some((name.clone(), args_delta.clone())),
                _ => None,
            })
            .expect("a tool call");

        assert_eq!(call.0.as_deref(), Some(crate::policy::tool::MEMORY_WRITE));

        let args: serde_json::Value = serde_json::from_str(&call.1).expect("valid arguments");
        assert_eq!(args["kind"], "convention");
        assert_eq!(args["text"], REMEMBER_TEXT);
        // About the demo, never about the user. A scripted provider inventing a
        // preference would be putting words in somebody's mouth in the one
        // store that outlives every session.
        assert_eq!(args["source"], "agent/provider/fake.rs");
    }

    /// PLAN 7.3, Phase 13: the skill trigger walks a whole run — load, then
    /// return, then stop — so the runner can be seen working without a model.
    /// The three rounds are asserted together because what the trigger is
    /// demonstrating is the *sequence*, and each round is decided from what the
    /// last one came back with.
    #[tokio::test]
    async fn the_skill_trigger_loads_a_runbook_and_then_closes_the_run() {
        /// The one call a round made, if it made one.
        fn call_of(events: &[ModelEvent]) -> Option<(String, String)> {
            events.iter().find_map(|event| match event {
                ModelEvent::ToolCallDelta {
                    name, args_delta, ..
                } => Some((name.clone()?, args_delta.clone())),
                _ => None,
            })
        }

        let provider = FakeProvider::instant();
        let catalog = "Skills you may run.\n\n- `inbox.triage` (v1, this workspace) — sorts an \
                       item into the board.";

        // Round one: the catalog names a runbook, so it asks for that one.
        let mut request = request_saying("please /skill");
        if let Some(WireMessage::System { content }) = request.messages.first_mut() {
            content.push_str("\n\n");
            content.push_str(catalog);
        }
        let opened = call_of(&drain(&provider, request.clone()).await).expect("a call");
        assert_eq!(opened.0, crate::policy::tool::SKILL_RUN);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&opened.1).expect("arguments"),
            serde_json::json!({ "name": "inbox.triage" })
        );

        // Round two: the run is open, so it closes it — and honestly, with a
        // `blocked`, because it did not do the work.
        request.messages.push(WireMessage::Tool {
            tool_call_id: "call_1".to_owned(),
            content: r#"{"ok":true,"tool":"skill_run","meta":{"skill":"inbox.triage"}}"#.to_owned(),
        });
        let closed = call_of(&drain(&provider, request.clone()).await).expect("a call");
        assert_eq!(closed.0, crate::policy::tool::SKILL_RETURN);
        let args: serde_json::Value = serde_json::from_str(&closed.1).expect("arguments");
        assert_eq!(args["status"], "blocked");
        assert!(
            args["open_questions"]
                .as_array()
                .is_some_and(|questions| !questions.is_empty()),
            "a blocked with nothing to answer would be refused by the runner: {args}"
        );

        // Round three: the run is closed, so it says so and stops.
        request.messages.push(WireMessage::Tool {
            tool_call_id: "call_2".to_owned(),
            content: r#"{"ok":true,"tool":"skill_return","meta":{"skill":"inbox.triage"}}"#
                .to_owned(),
        });
        let finished = drain(&provider, request).await;
        assert!(call_of(&finished).is_none(), "the loop ends in a word");
        assert!(text_of(&finished).contains("audit log"), "{:?}", finished);
    }

    /// An identity granted no skills has no catalog in its system message, and
    /// the trigger says so rather than inventing a name to call.
    #[tokio::test]
    async fn the_skill_trigger_with_no_catalog_asks_for_nothing() {
        let events = drain(&FakeProvider::instant(), request_saying("/skill please")).await;

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ModelEvent::ToolCallDelta { .. })),
            "nothing to run means no call: {events:?}"
        );
        assert!(text_of(&events).contains("granted no skills"));
    }

    /// The trigger fires once per turn, not once per round. A provider that
    /// asked again after the tool answered would burn all eight rounds on the
    /// same file and look exactly like a gate that is not holding.
    #[tokio::test]
    async fn the_trigger_does_not_fire_again_once_the_call_is_answered() {
        let mut request = request_saying("please /write something");
        request.messages.push(WireMessage::Tool {
            tool_call_id: "call_1".to_owned(),
            content: r#"{"ok":true}"#.to_owned(),
        });

        let events = drain(&FakeProvider::instant(), request).await;

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ModelEvent::ToolCallDelta { .. })),
            "the second round explains itself instead of asking again"
        );
    }

    #[tokio::test]
    async fn an_ordinary_message_never_reaches_for_a_tool() {
        let events = drain(&FakeProvider::instant(), request_saying("hello there")).await;

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ModelEvent::ToolCallDelta { .. })),
            "the fake model only touches the disk when asked to by name"
        );
    }

    #[tokio::test]
    async fn the_model_id_is_what_the_turn_reports() {
        let provider = FakeProvider::new();
        assert_eq!(provider.model(), FAKE_MODEL);

        let request = request_saying("x");
        assert_eq!(request.model, FAKE_MODEL);
        assert!(matches!(request.messages[0], WireMessage::System { .. }));
    }
}
