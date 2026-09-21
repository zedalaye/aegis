use super::*;

use std::sync::Mutex;

use serde_json::json;
use tempfile::TempDir;

/// PLAN 7.18: without a TypeSafe key the decision tools are not shown,
/// and nothing else changes.
#[test]
fn decision_tools_are_offered_only_with_a_client() {
    let held: Vec<String> = ["fs_read", "jev_eval", "jev_ask"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(offered_tools(&held, false), ["fs_read"]);
    assert_eq!(offered_tools(&held, true), held);
}

use crate::agent::provider::FakeProvider;
use crate::agent::wire::ModelEvent;
use crate::approval::{ApprovalRequest, Decision as Answered};
use crate::policy::tool;
use crate::policy::Grant;

/// The turn every fixture registers, so `plan()` and the registry agree.
const TURN_ID: &str = "t1";

/// An [`EventSink`] that keeps everything, for assertions.
#[derive(Debug, Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Recorder {
    fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn names(&self) -> Vec<&'static str> {
        self.events().iter().map(Event::name).collect()
    }

    /// The concatenated text of every `turn:delta`.
    fn streamed(&self) -> String {
        self.events()
            .iter()
            .filter_map(|event| match event {
                Event::TurnDelta(delta) => Some(delta.text.clone()),
                _ => None,
            })
            .collect()
    }

    fn finished(&self) -> Option<StopReason> {
        self.events().iter().find_map(|event| match event {
            Event::TurnFinished(finished) => Some(finished.stop_reason),
            _ => None,
        })
    }
}

impl EventSink for Recorder {
    fn emit(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }
}

/// A workspace, the stores over it, and a session ready to send into.
struct Fixture {
    _dir: TempDir,
    workspace: PathBuf,
    sessions: SessionStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
    sink: Recorder,
    session_id: String,
    captures: PathBuf,
    /// An empty skill library; the runner is tested elsewhere.
    library: PathBuf,
    /// An empty memory store.
    memories: MemoryStore,
    connectors: Connectors,
    /// Where an ask nobody answers is filed (PLAN 7.22). Only the turns built
    /// by [`Fixture::parking`] reach it.
    parked: crate::store::ParkedStore,
    /// Notifications this fixture drops on the floor.
    notifier: crate::notify::Quiet,
    /// The built-in identity, which holds every tool.
    agent: Agent,
    /// Where a metered turn's rounds are charged (PLAN 7.26).
    spend: crate::store::SpendLedger,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace).expect("workspace dir");

        let captures = data.join("captures");
        let sessions = SessionStore::load(&data);
        let session_id = sessions
            .create("p1", None, crate::store::DEFAULT_AGENT_ID)
            .expect("session")
            .id;

        // Registered as `session_send` does; tests read the state back.
        let turns = TurnRegistry::new();
        turns.begin(&session_id, TURN_ID).expect("a free session");

        Self {
            workspace: dunce::canonicalize(&workspace).expect("canonical workspace"),
            _dir: dir,
            sessions,
            turns,
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(&data),
            sink: Recorder::default(),
            session_id,
            captures,
            library: data.join("skills"),
            memories: MemoryStore::load(&data),
            connectors: Connectors::new(),
            parked: crate::store::ParkedStore::load(&data),
            notifier: crate::notify::Quiet,
            agent: Agent::builtin(),
            spend: crate::store::SpendLedger::load(&data),
        }
    }

    /// Somewhere for this session's turns to park (PLAN 7.22). No routine: a
    /// session someone opened parks only when a dialog expires.
    fn parking<'a>(&'a self, parks: &'a crate::park::Parks) -> Parking<'a> {
        Parking {
            store: &self.parked,
            notifier: &self.notifier,
            project_id: "p1",
            routine_id: "",
            routine_name: "",
            skill: "",
            parks,
        }
    }

    fn plan(&self) -> TurnPlan {
        TurnPlan {
            session_id: self.session_id.clone(),
            turn_id: TURN_ID.to_owned(),
            workspace: Some(self.workspace.clone()),
            exec_host: None,
        }
    }

    fn turn<'a>(&'a self, provider: &'a dyn Provider) -> Turn<'a> {
        Turn {
            agent: &self.agent,
            sessions: &self.sessions,
            turns: &self.turns,
            grants: &self.grants,
            approvals: &self.approvals,
            audit: &self.audit,
            provider,
            sink: &self.sink,
            self_exe: None,
            captures: &self.captures,
            skills: &self.library,
            memories: &self.memories,
            connectors: &self.connectors,
            standing: Standing::Own(None),
            unattended: None,
            parking: None,
            decision: None,
            meter: None,
        }
    }

    /// A meter for this fixture's turn at $10 per million reply tokens, or
    /// with no price (PLAN 7.26).
    fn meter(&self, priced: bool) -> Meter<'_> {
        let price = crate::store::ModelPrice {
            model: "fake".to_owned(),
            input: 0,
            output: 10 * crate::store::ledger::DOLLAR,
            cache_read: None,
            cache_write: None,
        };
        Meter::new(
            &self.spend,
            &self.notifier,
            spend::Tariff {
                provider_id: "default".to_owned(),
                model: "fake".to_owned(),
                price: priced.then_some(price),
                max_output_tokens: None,
            },
            spend::Payer {
                project_id: "p1",
                session_id: &self.session_id,
                turn_id: TURN_ID,
                agent: &self.agent,
                routine: None,
                run_is_session: false,
            },
        )
    }

    /// The approval the turn is blocked on, polled the way a click finds it.
    async fn pending(&self) -> ApprovalRequest {
        for _ in 0..200 {
            if let Some(request) = self
                .approvals
                .list(Some(&self.session_id))
                .into_iter()
                .next()
            {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("no approval was raised");
    }

    /// Answers whatever the turn is waiting on, the way the command does.
    async fn answer(&self, decision: Answered) {
        let request = self.pending().await;
        self.approvals
            .resolve(&request.request_id, decision, &self.grants)
            .expect("the request is open");
    }

    fn say(&self, text: &str) {
        self.sessions
            .append(&self.session_id, Message::user(text), SessionState::Running)
            .expect("append");
    }

    fn transcript(&self) -> Vec<Message> {
        self.sessions.messages(&self.session_id).expect("messages")
    }
}

/// Streams a tool call, then whatever the model says afterwards.
fn tool_call_script(id: &str, name: &str, args: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(id.to_owned()),
            name: Some(name.to_owned()),
            args_delta: args.to_owned(),
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

#[tokio::test]
async fn a_plain_turn_streams_persists_and_finishes() {
    let fx = Fixture::new();
    fx.say("hello");

    let provider = FakeProvider::instant();
    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(reason, StopReason::Stop);
    assert_eq!(fx.sink.finished(), Some(StopReason::Stop));

    let names = fx.sink.names();
    assert_eq!(names.first(), Some(&"turn:started"));
    assert_eq!(names.last(), Some(&"turn:finished"));
    assert!(names.contains(&"turn:message"));
    assert!(names.contains(&"session:updated"));

    // What was streamed is what was stored: the UI swaps its buffer for
    // the finalized message, and the two must agree.
    let transcript = fx.transcript();
    let reply = transcript.last().expect("a reply");
    assert_eq!(reply.role, crate::store::Role::Assistant);
    assert_eq!(fx.sink.streamed(), reply.text);
    assert!(reply.text.contains("hello"), "{}", reply.text);
}

/// Deltas are frames, not tokens: the WebView is woken about twenty times
/// a second however fast the provider goes.
#[tokio::test]
async fn deltas_are_coalesced_into_frames() {
    let fx = Fixture::new();
    fx.say("hello");

    let provider = FakeProvider::instant();
    fx.turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    let deltas: Vec<u32> = fx
        .sink
        .events()
        .iter()
        .filter_map(|event| match event {
            Event::TurnDelta(delta) => Some(delta.seq),
            _ => None,
        })
        .collect();

    assert!(!deltas.is_empty(), "something streamed");
    let tokens = fx
        .transcript()
        .last()
        .expect("reply")
        .text
        .split(' ')
        .count();
    assert!(
        deltas.len() < tokens,
        "{} frames for {tokens} tokens is not coalescing",
        deltas.len()
    );

    // `seq` is monotonic from zero, which is what lets the UI drop
    // duplicates after a reload.
    let expected: Vec<u32> = (0..u32::try_from(deltas.len()).expect("small")).collect();
    assert_eq!(deltas, expected);
}

#[tokio::test]
async fn cancelling_stops_the_turn_and_keeps_what_was_said() {
    let fx = Fixture::new();
    fx.say("hello");

    // Pacing is on, so the cancel lands mid-stream rather than after.
    let provider = FakeProvider::new();
    let cancel = CancellationToken::new();

    let cancel_after = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        cancel_after.cancel();
    });

    let reason = fx.turn(&provider).run(&fx.plan(), &cancel).await;

    assert_eq!(reason, StopReason::Cancelled);
    assert_eq!(fx.sink.finished(), Some(StopReason::Cancelled));

    let transcript = fx.transcript();
    let reply = transcript.last().expect("a partial reply");
    assert_eq!(reply.role, crate::store::Role::Assistant);
    assert!(!reply.text.is_empty(), "what the user saw is kept");
    assert_eq!(
        fx.sink.streamed(),
        reply.text,
        "the stored partial is exactly what was streamed"
    );
}

#[tokio::test]
async fn a_cancel_before_the_first_token_ends_the_turn_immediately() {
    let fx = Fixture::new();
    fx.say("hello");

    let cancel = CancellationToken::new();
    cancel.cancel();

    let provider = FakeProvider::new();
    let reason = fx.turn(&provider).run(&fx.plan(), &cancel).await;

    assert_eq!(reason, StopReason::Cancelled);
    assert!(
        !fx.sink.names().contains(&"turn:message"),
        "nothing was said, so nothing is stored"
    );
}

#[tokio::test]
async fn a_tool_call_runs_under_policy_and_is_answered() {
    let fx = Fixture::new();
    std::fs::write(fx.workspace.join("a.txt"), "hello file").expect("write");
    fx.say("read a.txt");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_READ,
        r#"{"path":"a.txt"}"#,
    )]);

    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;
    assert_eq!(
        reason,
        StopReason::Stop,
        "the round after the tool finished"
    );

    let names = fx.sink.names();
    assert!(names.contains(&"tool:requested"), "{names:?}");
    assert!(names.contains(&"tool:started"), "{names:?}");
    assert!(names.contains(&"tool:finished"), "{names:?}");
    assert!(names.contains(&"audit:appended"), "{names:?}");

    let transcript = fx.transcript();
    let call = &transcript[1].tool_calls[0];
    assert_eq!(call.status, ToolCallStatus::Ok);
    assert!(call.summary.is_some());

    let answer = &transcript[2];
    assert_eq!(answer.role, crate::store::Role::Tool);
    assert_eq!(answer.tool_call_id.as_deref(), Some("call_1"));
    let envelope: serde_json::Value = serde_json::from_str(&answer.text).expect("an envelope");
    assert_eq!(envelope["ok"], json!(true));
    assert_eq!(envelope["content"], "hello file");

    // The audit log holds the call, whatever the UI did with the event.
    assert_eq!(fx.audit.tail(10, None).expect("tail").len(), 1);
}

/// A write inside the workspace is asked about. Allowing it once runs it,
/// and leaves nothing behind that would skip the next prompt.
#[tokio::test]
async fn an_allowed_call_runs_and_grants_nothing() {
    let fx = Fixture::new();
    fx.say("write a file");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_WRITE,
        r#"{"path":"new.txt","content":"x"}"#,
    )]);

    let answering = async {
        fx.answer(Answered::AllowOnce).await;
    };
    let turn = fx.turn(&provider);
    let plan = fx.plan();
    let cancel = CancellationToken::new();
    let (reason, ()) = tokio::join!(turn.run(&plan, &cancel), answering);

    assert_eq!(reason, StopReason::Stop);
    assert_eq!(
        std::fs::read_to_string(fx.workspace.join("new.txt")).expect("the file"),
        "x"
    );

    let names = fx.sink.names();
    assert!(names.contains(&"tool:approval_required"), "{names:?}");
    assert!(names.contains(&"tool:approval_resolved"), "{names:?}");
    assert!(names.contains(&"tool:started"), "{names:?}");

    let transcript = fx.transcript();
    assert_eq!(transcript[1].tool_calls[0].status, ToolCallStatus::Ok);

    let audit = fx.audit.tail(10, None).expect("tail");
    assert_eq!(audit[0].decision, AuditDecision::AllowOnce);
    assert_eq!(audit[0].outcome, Outcome::Ok);

    assert!(
        fx.grants.list(&fx.session_id).is_empty(),
        "allow-once must not quietly become allow-session"
    );
    assert!(fx.approvals.is_empty(), "the request was consumed");
}

/// PLAN 6, Phase 6 exit: a denial lands in the transcript as `E_DENIED`
/// without aborting the turn.
#[tokio::test]
async fn a_denial_is_a_result_and_the_turn_carries_on() {
    let fx = Fixture::new();
    fx.say("write a file");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_WRITE,
        r#"{"path":"new.txt","content":"x"}"#,
    )]);

    let answering = async {
        fx.answer(Answered::Deny).await;
    };
    let turn = fx.turn(&provider);
    let plan = fx.plan();
    let cancel = CancellationToken::new();
    let (reason, ()) = tokio::join!(turn.run(&plan, &cancel), answering);

    assert_eq!(reason, StopReason::Stop, "a denial does not end the turn");
    assert!(
        !fx.workspace.join("new.txt").exists(),
        "a denied write must not touch the disk"
    );
    assert!(
        !fx.sink.names().contains(&"tool:started"),
        "nothing was started"
    );

    let transcript = fx.transcript();
    assert_eq!(transcript[1].tool_calls[0].status, ToolCallStatus::Denied);

    let envelope: serde_json::Value =
        serde_json::from_str(&transcript[2].text).expect("an envelope");
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["error"]["code"], "E_DENIED");

    // The model is told it was the user, and told not to repeat it.
    let message = envelope["error"]["message"]
        .as_str()
        .expect("a message")
        .to_owned();
    assert!(message.contains("user refused"), "{message}");

    let audit = fx.audit.tail(10, None).expect("tail");
    assert_eq!(audit[0].decision, AuditDecision::Deny);
    assert_eq!(audit[0].outcome, Outcome::Denied);
}

/// The second half of `allow_session`: the grant is recorded, and the call
/// behind it is not asked about again.
#[tokio::test]
async fn allowing_for_the_session_stops_the_next_prompt() {
    let fx = Fixture::new();
    fx.say("write two files");

    let provider = FakeProvider::scripted(vec![
        tool_call_script(
            "call_1",
            tool::FS_WRITE,
            r#"{"path":"one.txt","content":"1"}"#,
        ),
        tool_call_script(
            "call_2",
            tool::FS_WRITE,
            r#"{"path":"two.txt","content":"2"}"#,
        ),
    ]);

    let answering = async {
        fx.answer(Answered::AllowSession).await;
    };
    let turn = fx.turn(&provider);
    let plan = fx.plan();
    let cancel = CancellationToken::new();
    let (_reason, ()) = tokio::join!(turn.run(&plan, &cancel), answering);

    assert_eq!(fx.grants.list(&fx.session_id), vec![Grant::FsWrite]);
    assert!(fx.workspace.join("one.txt").is_file());
    assert!(
        fx.workspace.join("two.txt").is_file(),
        "the second write was covered by the grant"
    );

    let asked = fx
        .sink
        .names()
        .iter()
        .filter(|name| **name == "tool:approval_required")
        .count();
    assert_eq!(asked, 1, "the user was asked once, not twice");

    let audit = fx.audit.tail(10, None).expect("tail");
    assert_eq!(audit.len(), 2);
    // Newest first: the covered call, then the one that was approved.
    assert_eq!(audit[0].decision, AuditDecision::Auto);
    assert_eq!(audit[1].decision, AuditDecision::AllowSession);
}

/// Stopping a turn while its dialog is open must not leave the call
/// unanswered, and must not record it as a refusal — nobody said no.
#[tokio::test]
async fn cancelling_while_waiting_abandons_the_call() {
    let fx = Fixture::new();
    fx.say("write a file");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_WRITE,
        r#"{"path":"new.txt","content":"x"}"#,
    )]);

    let cancel = CancellationToken::new();
    let stopping = async {
        fx.pending().await;
        cancel.cancel();
    };
    let turn = fx.turn(&provider);
    let plan = fx.plan();
    let (reason, ()) = tokio::join!(turn.run(&plan, &cancel), stopping);

    assert_eq!(reason, StopReason::Cancelled);
    assert!(!fx.workspace.join("new.txt").exists());
    assert!(
        fx.approvals.is_empty(),
        "a cancelled turn leaves no dialog behind"
    );

    let transcript = fx.transcript();
    assert_eq!(
        transcript[1].tool_calls[0].status,
        ToolCallStatus::Cancelled
    );
    let envelope: serde_json::Value =
        serde_json::from_str(&transcript[2].text).expect("an envelope");
    assert_eq!(envelope["error"]["code"], "E_CANCELLED");

    let resolved = fx.sink.events().into_iter().find_map(|event| match event {
        Event::ToolApprovalResolved(resolved) => Some(resolved),
        _ => None,
    });
    assert_eq!(
        resolved.expect("the dialog was closed").resolved_by,
        ResolvedBy::Policy
    );
}

/// An unanswered approval is refused after [`APPROVAL_TTL`] and the turn
/// carries on. A paused clock skips the wait.
#[tokio::test(start_paused = true)]
async fn an_unanswered_approval_expires_and_the_turn_carries_on() {
    let fx = Fixture::new();
    fx.say("write a file");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_WRITE,
        r#"{"path":"new.txt","content":"x"}"#,
    )]);

    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(reason, StopReason::Stop, "the turn ends cleanly");
    assert!(
        !fx.workspace.join("new.txt").exists(),
        "an expired approval must not run the call"
    );
    assert!(fx.approvals.is_empty(), "the request was withdrawn");

    let transcript = fx.transcript();
    assert_eq!(transcript[1].tool_calls[0].status, ToolCallStatus::Denied);
    let envelope: serde_json::Value =
        serde_json::from_str(&transcript[2].text).expect("an envelope");
    assert_eq!(envelope["error"]["code"], "E_DENIED");

    let resolved = fx.sink.events().into_iter().find_map(|event| match event {
        Event::ToolApprovalResolved(resolved) => Some(resolved),
        _ => None,
    });
    assert_eq!(
        resolved.expect("the dialog was closed").resolved_by,
        ResolvedBy::Timeout
    );
}

/// `IDEAS.md` § 5: five minutes with nobody at the screen threw the expensive
/// part away. With somewhere to park (PLAN 7.22) the dialog still expires, but
/// the question — and the arguments the model spent its turn writing — survive
/// it, and the model is told the difference.
#[tokio::test(start_paused = true)]
async fn an_expired_approval_parks_rather_than_being_thrown_away() {
    let fx = Fixture::new();
    fx.say("write a file");
    let parks = crate::park::Parks::new();
    let parking = fx.parking(&parks);

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_WRITE,
        r#"{"path":"new.txt","content":"x"}"#,
    )]);

    let reason = Turn {
        parking: Some(&parking),
        ..fx.turn(&provider)
    }
    .run(&fx.plan(), &CancellationToken::new())
    .await;

    assert_eq!(reason, StopReason::Stop, "the turn ends cleanly");
    assert!(
        !fx.workspace.join("new.txt").exists(),
        "a parked call has not run"
    );
    assert!(fx.approvals.is_empty(), "the dialog was withdrawn");

    let envelope: serde_json::Value =
        serde_json::from_str(&fx.transcript()[2].text).expect("an envelope");
    assert_eq!(
        envelope["error"]["code"], "E_PARKED",
        "a parked call is not a refused one"
    );

    let waiting = fx.parked.list(None);
    assert_eq!(waiting.len(), 1, "the question is on the board");
    assert_eq!(waiting[0].cause, crate::store::ParkCause::Expired);
    assert_eq!(waiting[0].tool, tool::FS_WRITE);
    assert_eq!(
        waiting[0].grant,
        Some(Grant::FsWrite),
        "answering it standing is still on offer"
    );
    assert_eq!(parks.ids().len(), 1);
}

/// While a dialog is open the session is not "working" — it is waiting for
/// the person looking at it, and the sidebar has to say so.
#[tokio::test]
async fn the_session_reads_as_awaiting_approval_while_a_dialog_is_open() {
    let fx = Fixture::new();
    fx.say("write a file");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_WRITE,
        r#"{"path":"new.txt","content":"x"}"#,
    )]);

    let watching = async {
        fx.pending().await;
        let state = fx.turns.state_of(&fx.session_id);
        fx.answer(Answered::Deny).await;
        state
    };
    let turn = fx.turn(&provider);
    let plan = fx.plan();
    let cancel = CancellationToken::new();
    let (_reason, state) = tokio::join!(turn.run(&plan, &cancel), watching);

    assert_eq!(state, SessionState::AwaitingApproval);
    assert_eq!(
        fx.turns.state_of(&fx.session_id),
        SessionState::Running,
        "the turn goes back to working once it is answered"
    );

    let awaiting = fx.sink.events().into_iter().any(|event| match event {
        Event::SessionUpdated(summary) => summary.state == SessionState::AwaitingApproval,
        _ => false,
    });
    assert!(awaiting, "the sidebar was told the session is blocked");
}

/// PLAN 3.2: an unresolvable path is refused with no approval, and the turn
/// continues. (A path outside the workspace would ask, not refuse.)
#[tokio::test]
async fn a_hard_denial_is_a_result_and_the_turn_continues() {
    let fx = Fixture::new();
    fx.say("read a file whose name never resolves");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_READ,
        r#"{"path":"   "}"#,
    )]);

    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(reason, StopReason::Stop, "a denial does not end the turn");
    assert!(
        !fx.sink.names().contains(&"tool:approval_required"),
        "a hard denial is never offered to the user"
    );

    let transcript = fx.transcript();
    assert_eq!(
        transcript[1].tool_calls[0].status,
        ToolCallStatus::Denied,
        "a refusal reads as refused, not as a call that ran and failed"
    );
    let envelope: serde_json::Value =
        serde_json::from_str(&transcript[2].text).expect("an envelope");
    assert_eq!(envelope["ok"], json!(false));

    // The refusal is audited: a call that never ran is still a call that
    // was made.
    let audit = fx.audit.tail(10, None).expect("tail");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].outcome, Outcome::Denied);
}

/// Arguments that never became valid JSON are answered, never executed
/// (PLAN 4.1) — the model can then correct itself.
#[tokio::test]
async fn unparseable_arguments_become_a_tool_message_not_a_dead_turn() {
    let fx = Fixture::new();
    fx.say("read something");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_1",
        tool::FS_READ,
        r#"{"path":"a.txt"#,
    )]);

    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(reason, StopReason::Stop);
    assert!(!fx.sink.names().contains(&"tool:started"), "nothing ran");

    let transcript = fx.transcript();
    let envelope: serde_json::Value =
        serde_json::from_str(&transcript[2].text).expect("an envelope");
    assert_eq!(envelope["error"]["code"], "E_TOOL_FAILED");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("not valid JSON"),
        "{envelope}"
    );
}

#[tokio::test]
async fn a_provider_failure_ends_the_turn_with_an_error() {
    let fx = Fixture::new();
    fx.say("hello");

    let provider = FakeProvider::scripted(vec![vec![
        ModelEvent::TextDelta {
            text: "starting".to_owned(),
        },
        ModelEvent::Error {
            code: ErrorCode::ProviderHttp.as_str().to_owned(),
            message: "the provider answered 503".to_owned(),
            retryable: true,
        },
    ]]);

    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(reason, StopReason::Error);
    assert_eq!(resting_state(reason), SessionState::Error);

    let error = fx
        .sink
        .events()
        .into_iter()
        .find_map(|event| match event {
            Event::TurnError(error) => Some(error),
            _ => None,
        })
        .expect("a turn:error");
    assert_eq!(error.code, "E_PROVIDER_HTTP");
    assert!(error.retryable);

    // `turn:error` is always followed by `turn:finished`, so a UI that only
    // tracks the lifecycle still re-enables its composer.
    assert_eq!(fx.sink.names().last(), Some(&"turn:finished"));
    assert!(fx
        .transcript()
        .last()
        .expect("partial")
        .text
        .contains("starting"));
}

/// A stream that stops without saying why produced a truncated reply.
/// Reporting it as a clean stop would hide that.
#[tokio::test]
async fn a_stream_that_ends_without_finishing_is_an_error() {
    let fx = Fixture::new();
    fx.say("hello");

    let provider = FakeProvider::scripted(vec![vec![ModelEvent::TextDelta {
        text: "half a sen".to_owned(),
    }]]);

    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(reason, StopReason::Error);
    let error = fx
        .sink
        .events()
        .into_iter()
        .find_map(|event| match event {
            Event::TurnError(error) => Some(error),
            _ => None,
        })
        .expect("a turn:error");
    assert_eq!(error.code, "E_PROVIDER_PARSE");
}

/// PLAN 7.16: repeating the same call is a loop, not a request for the
/// user to type continue. The third identical round is refused; the two
/// before it ran.
#[tokio::test]
async fn a_repeated_call_stops_as_a_loop() {
    let fx = Fixture::new();
    std::fs::write(fx.workspace.join("a.txt"), "x").expect("write");
    fx.say("keep reading");

    // LOOP_STREAK identical rounds, plus one more the wrap-up must not run.
    let script = (0..=LOOP_STREAK)
        .map(|round| {
            tool_call_script(
                &format!("call_{round}"),
                tool::FS_READ,
                r#"{"path":"a.txt"}"#,
            )
        })
        .collect();

    let provider = FakeProvider::scripted(script);
    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(reason, StopReason::Stop);

    let envelopes: Vec<serde_json::Value> = fx
        .transcript()
        .into_iter()
        .filter(|message| message.role == crate::store::Role::Tool)
        .map(|message| serde_json::from_str(&message.text).expect("an envelope"))
        .collect();
    let looped = envelopes
        .iter()
        .filter(|envelope| envelope["error"]["code"] == "E_TOOL_LOOP")
        .count();
    assert!(
        looped >= 1,
        "the repeated round is refused as a loop: {envelopes:?}"
    );
    assert!(
        envelopes.iter().all(|envelope| {
            envelope["error"]["code"] != "E_TOOL_LOOP"
                || !envelope["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains("continue")
        }),
        "the recovery is finish, not a human continue: {envelopes:?}"
    );

    let executed = fx
        .sink
        .names()
        .iter()
        .filter(|name| **name == "tool:started")
        .count();
    assert_eq!(
        executed,
        usize::try_from(LOOP_STREAK.saturating_sub(1)).expect("small"),
        "only the rounds before the loop ran"
    );
}

/// Distinct work past the old eight-round cap keeps running. The ceiling
/// is a bill bound, not "stop and wait for continue".
#[tokio::test]
async fn progress_past_the_old_cap_keeps_running() {
    let fx = Fixture::new();
    fx.say("read them");

    let old_cap = 8u32;
    let past = old_cap + 2;
    for n in 0..past {
        std::fs::write(fx.workspace.join(format!("a{n}.txt")), "x").expect("write");
    }
    let script = (0..past)
        .map(|n| {
            tool_call_script(
                &format!("call_{n}"),
                tool::FS_READ,
                &format!(r#"{{"path":"a{n}.txt"}}"#),
            )
        })
        .collect();

    let provider = FakeProvider::scripted(script);
    fx.turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    let refused = fx.transcript().into_iter().any(|message| {
        serde_json::from_str::<serde_json::Value>(&message.text).is_ok_and(|envelope| {
            envelope["error"]["code"] == "E_TOO_MANY_TOOL_ROUNDS"
                || envelope["error"]["code"] == "E_TOOL_LOOP"
        })
    });
    assert!(!refused, "ten distinct reads are progress, not a halt");

    let executed = fx
        .sink
        .names()
        .iter()
        .filter(|name| **name == "tool:started")
        .count();
    assert_eq!(
        executed,
        usize::try_from(past).expect("small"),
        "every distinct round ran"
    );
}

/// Writes a runbook into the library and grants it: writing alone grants
/// nothing.
fn grant_runbook(fx: &mut Fixture, name: &str) {
    let dir = fx.library.join(name);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(dir.join("SKILL.md"), crate::skills::TRIAGE_SEED).expect("runbook");
    fx.agent.skills = vec![name.to_owned()];
}

/// After a loop halt the model gets one wrap-up request, so it can
/// summarize instead of leaving the user to type continue (PLAN 7.16).
#[tokio::test]
async fn a_halt_gives_the_model_a_wrap_up_round() {
    let fx = Fixture::new();
    std::fs::write(fx.workspace.join("a.txt"), "x").expect("write");
    fx.say("keep reading");

    let mut script: Vec<_> = (0..LOOP_STREAK)
        .map(|round| {
            tool_call_script(
                &format!("call_{round}"),
                tool::FS_READ,
                r#"{"path":"a.txt"}"#,
            )
        })
        .collect();
    script.push(vec![
        ModelEvent::TextDelta {
            text: "stopped; here is what I have".to_owned(),
        },
        ModelEvent::Finish {
            reason: StopReason::Stop,
            usage: None,
        },
    ]);

    let provider = FakeProvider::scripted(script);
    fx.turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    let transcript = fx.transcript();
    let last = transcript.last().expect("a final assistant message");
    assert_eq!(last.role, crate::store::Role::Assistant);
    assert!(
        last.text.contains("what I have"),
        "the wrap-up round reached the transcript: {}",
        last.text
    );
}

/// A turn that ends mid-run hands the run to the next one, so the
/// procedure the cap interrupted resumes under its own name
/// (`IDEAS.md` § 10).
#[tokio::test]
async fn a_turn_that_ends_mid_run_carries_it_to_the_next_turn() {
    let mut fx = Fixture::new();
    grant_runbook(&mut fx, "inbox.triage");
    fx.say("triage it");

    let provider = FakeProvider::scripted(vec![tool_call_script(
        "call_open",
        tool::SKILL_RUN,
        r#"{"name":"inbox.triage"}"#,
    )]);
    fx.turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(
        fx.turns.open_run(&fx.session_id).as_deref(),
        Some("inbox.triage"),
        "the run outlives the turn that opened it"
    );
}

/// Except when the user pressed Stop, which is the clearest statement
/// there is that the conversation has moved on.
#[tokio::test]
async fn a_cancelled_turn_closes_the_run_rather_than_carrying_it() {
    let mut fx = Fixture::new();
    grant_runbook(&mut fx, "inbox.triage");
    fx.say("triage it");

    let provider = FakeProvider::scripted(vec![
        tool_call_script("call_open", tool::SKILL_RUN, r#"{"name":"inbox.triage"}"#),
        tool_call_script(
            "call_write",
            tool::FS_WRITE,
            r#"{"path":"new.txt","content":"x"}"#,
        ),
    ]);

    // Cancelled on the write's dialog, where a person would press Stop.
    let cancel = CancellationToken::new();
    let stopping = async {
        fx.pending().await;
        cancel.cancel();
    };
    let turn = fx.turn(&provider);
    let plan = fx.plan();
    let (reason, ()) = tokio::join!(turn.run(&plan, &cancel), stopping);

    assert_eq!(reason, StopReason::Cancelled);
    assert_eq!(
        fx.turns.open_run(&fx.session_id),
        None,
        "Stop ends the run, not just the turn"
    );
}

/// Every tool call must end up with a `tool` message, whatever happened —
/// an unanswered one makes every later request in the session invalid.
#[tokio::test]
async fn every_call_is_answered_however_the_turn_ends() {
    let fx = Fixture::new();
    fx.say("do three things");

    let provider = FakeProvider::scripted(vec![vec![
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some("call_a".to_owned()),
            name: Some(tool::FS_LIST.to_owned()),
            args_delta: r#"{"path":"."}"#.to_owned(),
            thought_signature: None,
        },
        ModelEvent::ToolCallDelta {
            index: 1,
            id: Some("call_b".to_owned()),
            name: Some(tool::FS_READ.to_owned()),
            args_delta: r#"{"path":"nope.txt"}"#.to_owned(),
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]]);

    fx.turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    let transcript = fx.transcript();
    let answered: Vec<&str> = transcript
        .iter()
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    assert_eq!(answered, vec!["call_a", "call_b"]);
}

/// A large `fs_write` streams no text; drafting events show the call is
/// still being written.
#[tokio::test]
async fn a_call_being_written_reports_how_far_it_has_got() {
    let fx = Fixture::new();
    fx.say("write the file");

    // Three fragments of one call's arguments, as a real stream sends
    // them: the name arrives once, the rest is content.
    let provider = FakeProvider::scripted(vec![vec![
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some("call_a".to_owned()),
            name: Some(tool::FS_LIST.to_owned()),
            args_delta: r#"{"pa"#.to_owned(),
            thought_signature: None,
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            args_delta: r#"th":"#.to_owned(),
            thought_signature: None,
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            args_delta: r#""."}"#.to_owned(),
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]]);

    fx.turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    let drafts: Vec<ToolDrafting> = fx
        .sink
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::ToolDrafting(draft) => Some(draft),
            _ => None,
        })
        .collect();

    let last = drafts.last().expect("the call was reported as it arrived");
    assert_eq!(
        last.bytes,
        r#"{"path":"."}"#.len() as u64,
        "the count is the whole arguments string, fragments summed"
    );
    assert_eq!(last.tool.as_deref(), Some(tool::FS_LIST));
    assert_eq!(last.index, 0);
}

/// An unchanged drafting size is not re-reported on every frame.
#[test]
fn an_unchanged_count_stays_quiet() {
    let fx = Fixture::new();
    let plan = fx.plan();
    let provider = FakeProvider::new();
    let turn = fx.turn(&provider);

    let mut draft = Drafting::new(0);
    draft.tool = Some(tool::FS_WRITE.to_owned());
    draft.bytes = 4_096;
    let mut seq = 0;

    turn.emit_drafting(&plan, &mut draft, &mut seq);
    turn.emit_drafting(&plan, &mut draft, &mut seq);

    assert_eq!(seq, 1, "the second call had nothing new to say");
    assert_eq!(
        fx.sink
            .names()
            .iter()
            .filter(|name| **name == crate::agent::event::name::TOOL_DRAFTING)
            .count(),
        1
    );

    draft.bytes = 8_192;
    turn.emit_drafting(&plan, &mut draft, &mut seq);
    assert_eq!(seq, 2, "a count that moved is reported");
}

#[test]
fn a_finished_turn_leaves_the_session_in_a_drawable_state() {
    assert_eq!(resting_state(StopReason::Stop), SessionState::Idle);
    assert_eq!(resting_state(StopReason::Cancelled), SessionState::Idle);
    assert_eq!(resting_state(StopReason::Length), SessionState::Idle);
    assert_eq!(resting_state(StopReason::Error), SessionState::Error);
}

/// A tool round that reports `cents` worth of reply at [`Fixture::meter`]'s
/// price.
fn paid_call_script(id: &str, path: &str, cents: u64) -> Vec<ModelEvent> {
    let mut script = tool_call_script(id, tool::FS_READ, &format!(r#"{{"path":"{path}"}}"#));
    if let Some(ModelEvent::Finish { usage, .. }) = script.last_mut() {
        *usage = Some(Usage {
            prompt_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            completion_tokens: cents * 1_000,
            total_tokens: cents * 1_000,
        });
    }
    script
}

/// The `turn:error` codes this fixture's sink saw.
fn error_codes(fx: &Fixture) -> Vec<String> {
    fx.sink
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::TurnError(error) => Some(error.code),
            _ => None,
        })
        .collect()
}

/// PLAN 7.26: the round that crosses the cap is paid for, so its calls run;
/// nothing is sent after it — not even a wrap-up, which would resend the
/// whole context.
#[tokio::test]
async fn a_spend_cap_runs_the_round_it_crossed_on_and_sends_nothing_more() {
    let mut fx = Fixture::new();
    fx.agent.spend.per_run = Some(crate::store::ledger::DOLLAR / 2);
    std::fs::write(fx.workspace.join("a.txt"), "x").expect("write");
    std::fs::write(fx.workspace.join("b.txt"), "x").expect("write");
    fx.say("read both");

    let provider = FakeProvider::scripted(vec![
        paid_call_script("call_1", "a.txt", 30),
        paid_call_script("call_2", "b.txt", 30),
        vec![
            ModelEvent::TextDelta {
                text: "a wrap-up nobody paid for".to_owned(),
            },
            ModelEvent::Finish {
                reason: StopReason::Stop,
                usage: None,
            },
        ],
    ]);
    let meter = fx.meter(true);
    let reason = Turn {
        meter: Some(&meter),
        ..fx.turn(&provider)
    }
    .run(&fx.plan(), &CancellationToken::new())
    .await;

    assert_eq!(
        reason,
        StopReason::Stop,
        "the session rests idle, not in error"
    );
    let started = fx
        .sink
        .names()
        .iter()
        .filter(|name| **name == "tool:started")
        .count();
    assert_eq!(started, 2, "the round that crossed the cap ran");
    assert_eq!(error_codes(&fx), ["E_BUDGET"], "and the person is told");
    assert!(
        meter.halted().is_some(),
        "an unattended caller can tell why"
    );

    let transcript = fx.transcript();
    assert!(
        !transcript
            .iter()
            .any(|message| message.text.contains("nobody paid for")),
        "no request followed the crossing round"
    );
    assert_eq!(
        transcript.last().map(|message| message.role),
        Some(crate::store::Role::Tool),
        "the transcript ends on the results, so a next turn carries on from them"
    );
    assert_eq!(
        fx.spend.spent(crate::store::ledger::Scope::Turn(TURN_ID)),
        600_000
    );
    let recorded = fx.sessions.costs(&fx.session_id).expect("costs");
    assert_eq!(recorded.last().and_then(|cost| cost.micros), Some(600_000));
}

/// PLAN 7.26: a turn whose cap is already spent sends nothing.
#[tokio::test]
async fn a_spent_cap_sends_nothing() {
    let mut fx = Fixture::new();
    fx.agent.spend.per_day = Some(crate::store::ledger::DOLLAR);
    fx.say("hello");
    let account = crate::store::ledger::Account {
        turn_id: "earlier",
        session_id: "elsewhere",
        project_id: "p1",
        agent_id: &fx.agent.id,
        routine_id: "",
        provider_id: "default",
        model: "fake",
    };
    fx.spend
        .charge(&account, crate::store::ledger::DOLLAR, false)
        .expect("charged");

    let provider = FakeProvider::instant();
    let meter = fx.meter(true);
    let reason = Turn {
        meter: Some(&meter),
        ..fx.turn(&provider)
    }
    .run(&fx.plan(), &CancellationToken::new())
    .await;

    assert_eq!(reason, StopReason::Error);
    assert_eq!(error_codes(&fx), ["E_BUDGET"]);
    assert!(fx.sink.streamed().is_empty(), "no request went out");
}

/// PLAN 7.26: a cap that cannot be measured stops rather than passes.
#[tokio::test]
async fn a_cap_on_an_unpriced_model_sends_nothing() {
    let mut fx = Fixture::new();
    fx.agent.spend.per_run = Some(crate::store::ledger::DOLLAR);
    fx.say("hello");

    let provider = FakeProvider::instant();
    let meter = fx.meter(false);
    let reason = Turn {
        meter: Some(&meter),
        ..fx.turn(&provider)
    }
    .run(&fx.plan(), &CancellationToken::new())
    .await;

    assert_eq!(reason, StopReason::Error);
    assert_eq!(error_codes(&fx), ["E_BUDGET"]);
}

/// A halted run may still file its report in the wrap-up (PLAN 7.16): the
/// one call it can make. Refusing it made a scheduled run's halt a silence,
/// which pauses the routine.
#[tokio::test]
async fn a_halted_run_may_still_file_its_report() {
    let mut fx = Fixture::new();
    grant_runbook(&mut fx, "inbox.triage");
    std::fs::write(fx.workspace.join("a.txt"), "x").expect("write");
    fx.say("triage it");

    let mut script = vec![tool_call_script(
        "call_open",
        tool::SKILL_RUN,
        r#"{"name":"inbox.triage"}"#,
    )];
    script.extend((0..LOOP_STREAK).map(|round| {
        tool_call_script(
            &format!("call_{round}"),
            tool::FS_READ,
            r#"{"path":"a.txt"}"#,
        )
    }));
    script.push(tool_call_script(
        "call_return",
        tool::SKILL_RETURN,
        r#"{"status":"blocked","summary":"stuck reading a.txt","open_questions":["what next?"]}"#,
    ));

    let provider = FakeProvider::scripted(script);
    let reason = fx
        .turn(&provider)
        .run(&fx.plan(), &CancellationToken::new())
        .await;

    assert_eq!(reason, StopReason::Stop);
    let envelopes: Vec<serde_json::Value> = fx
        .transcript()
        .iter()
        .filter_map(|message| serde_json::from_str(&message.text).ok())
        .collect();
    let refusal = envelopes
        .iter()
        .find(|envelope| envelope["error"]["code"] == "E_TOOL_LOOP")
        .expect("the loop was refused");
    assert!(
        refusal["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("`skill_return`") && !text.contains("  ")),
        "the refusal names the one call left, in one clean sentence: {refusal}"
    );
    let returned = envelopes
        .iter()
        .find(|envelope| envelope["tool"] == "skill_return")
        .expect("the report was answered");
    assert_eq!(returned["ok"], true, "the report was filed: {returned}");
    assert_eq!(
        fx.turns.open_run(&fx.session_id),
        None,
        "and the run closed"
    );
}
