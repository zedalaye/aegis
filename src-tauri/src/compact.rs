//! Compaction: turning the older half of a transcript into *state*
//! (PLAN 7.3, Phase 14; PLAN 7.5; `COS.md` *Loop*).
//!
//! `COS.md` states the rule in one line: keep the last N turns raw, compact the
//! rest to state — goal, decisions, files, blockers — and after the compaction
//! re-inject retrieved memory rather than the novel. This module is the middle
//! clause. The other two are elsewhere and are worth naming, because between
//! them they are why this file contains no summarizer:
//!
//! * the raw tail is [`tail`], a slice of the stored transcript; and
//! * the re-injection is [`memories::prompt_block`](crate::store::memories),
//!   which is in *every* system message and so was never in the part that
//!   folds.
//!
//! ## Why nothing here calls a model
//!
//! PLAN 7.5 is explicit that treating compaction as an LLM vendor problem is a
//! mistake, and that it has to be coded as state. So it is derived, not
//! summarized: every line below comes from something that actually happened —
//! the first thing the user asked for, the paths `fs_write` actually wrote, the
//! programs `shell_exec` actually ran, the status a `skill_return` actually
//! reported, who a `handoff_delegate` actually handed work to and how the board
//! came back, the questions it left open. A summarizer would cost a request,
//! could hallucinate a file that was never written, and would produce a
//! different summary every time it ran over the same transcript. This produces
//! the same state for the same messages, always, in microseconds, and every
//! claim in it is checkable against the audit log.
//!
//! The cost of that honesty is that the state is *thin*: it holds what was
//! done, not what was reasoned. That is the intended trade. What was reasoned
//! and matters belongs in a file (`decisions/DECISIONS.md`), in a memory, or in
//! the last few turns, which are kept raw precisely so the thread of the
//! conversation is not what compaction destroys.
//!
//! ## Nothing is deleted
//!
//! A compaction records a *pointer* — the last message that folds — and the
//! state derived from everything up to it. The transcript on disk is untouched:
//! the user keeps scrolling through the whole conversation, the audit log still
//! answers for every call, and a later compaction re-derives its state from the
//! messages rather than from the previous state, so nothing accumulates drift.
//! What changes is only what reaches the model.
//!
//! ## The cut is always a turn boundary
//!
//! [`cut`] only ever splits immediately before a `user` message. That is what
//! keeps the request valid: the chat-completions API requires every tool call
//! in an assistant message to be answered by a `tool` message with a matching
//! id, and a cut in the middle of a round would strand one half of that pair.
//! It is also what makes the kept part readable — a turn, not a fragment.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::store::{Message, Role, ToolCallRecord, ToolCallStatus};

/// Turns kept raw at the end of the transcript.
///
/// A turn is a user message and everything that answered it. Four is enough to
/// keep the thread of what is being worked on right now — the correction, the
/// reply to it, and the two before — and few enough that the fold is worth
/// doing at all.
pub const KEEP_TURNS: usize = 4;

/// The size at which a session compacts itself without being asked.
///
/// Measured with [`weight`], which counts what the transcript costs *as a
/// request* rather than what it costs on disk. The number is a judgement about
/// context windows and not about correctness: nothing breaks above it, the
/// requests simply get expensive, and a session that has been running all week
/// should not quietly become the most expensive thing the app does.
pub const COMPACT_AT_BYTES: usize = 48 * 1024;

/// Most bytes of state a compaction may produce.
///
/// State that grew without limit would be the transcript again, in a worse
/// format. Everything below is capped individually as well; this is the
/// backstop.
pub const STATE_MAX_BYTES: usize = 2 * 1024;

/// Longest the goal line may be.
const GOAL_MAX_CHARS: usize = 240;

/// Longest one later request may be, and how many are kept.
const ASKED_MAX_CHARS: usize = 100;
const ASKED_MAX: usize = 3;

/// Most entries in the files, programs and skill-run lists.
const LIST_MAX: usize = 8;

/// Longest one blocker may be, and how many are kept.
const BLOCKER_MAX_CHARS: usize = 140;
const BLOCKERS_MAX: usize = 6;

/// The ledger a decision belongs in, so a write to it can be named as one.
///
/// Matched as a suffix on the path the model wrote, in either separator, so it
/// catches the relative path a model usually sends and the absolute one it
/// sometimes does.
const DECISIONS_SUFFIXES: [&str; 2] = ["decisions/DECISIONS.md", "decisions\\DECISIONS.md"];

/// What a compaction would record, before it is stamped and stored.
///
/// Returned by [`plan`] rather than written by it: deciding *what* to fold is
/// this module's, and persisting it — with a timestamp, under the store's lock
/// — is [`SessionStore::compact`](crate::store::SessionStore::compact)'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The last message that folds. Everything after it stays raw.
    pub through_message_id: String,
    /// How many messages folded.
    pub folded: u32,
    /// The state they became.
    pub state: String,
}

/// What a transcript costs as a request, in bytes.
///
/// Text plus the arguments of every tool call, because those are sent too and
/// a round of `fs_write` calls is most of what a long session weighs. It is a
/// proxy for tokens and is treated as one: the threshold it feeds is a
/// judgement, not a limit anything depends on.
pub fn weight(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| {
            message.text.len()
                + message
                    .tool_calls
                    .iter()
                    .map(|call| call.args_json.len() + call.tool.len())
                    .sum::<usize>()
        })
        .sum()
}

/// Where the transcript splits, as an index into `messages`.
///
/// The returned index is the first message that stays raw, and it always
/// addresses a `user` message — see the module note on why. `None` means there
/// is nothing to fold: fewer than `keep_turns + 1` turns, or a transcript whose
/// first turn is the only one old enough.
pub fn cut(messages: &[Message], keep_turns: usize) -> Option<usize> {
    let turns: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == Role::User)
        .map(|(at, _)| at)
        .collect();

    if turns.len() <= keep_turns {
        return None;
    }
    let at = turns[turns.len() - keep_turns];
    // A cut at zero folds nothing, which is not a compaction. Reachable when
    // the transcript opens with something other than a user message.
    (at > 0).then_some(at)
}

/// What folding this transcript would record, or `None` for nothing to do.
///
/// `force` is the difference between the button and the automatic path: forced,
/// the only question is whether there are enough turns; unforced, the
/// transcript also has to have grown past [`COMPACT_AT_BYTES`]. Both fold the
/// same amount, because "how much is kept" is a property of what is readable,
/// not of why the fold was asked for.
pub fn plan(messages: &[Message], force: bool) -> Option<Plan> {
    if !force && weight(messages) < COMPACT_AT_BYTES {
        return None;
    }

    let at = cut(messages, KEEP_TURNS)?;
    let folded = &messages[..at];

    Some(Plan {
        through_message_id: folded.last()?.id.clone(),
        folded: u32::try_from(folded.len()).unwrap_or(u32::MAX),
        state: state(folded),
    })
}

/// The messages that still reach the model.
///
/// `through` is the compaction's pointer, or `None` for a session that has
/// never been compacted. A pointer naming a message the transcript no longer
/// holds returns the whole transcript rather than nothing: a stale pointer is a
/// bug, and a bug that silently blanked a session's context would be one nobody
/// could see. Erring towards a request that is too long is the recoverable
/// direction.
pub fn tail<'a>(messages: &'a [Message], through: Option<&str>) -> &'a [Message] {
    let Some(through) = through else {
        return messages;
    };
    match messages.iter().position(|message| message.id == through) {
        Some(at) => &messages[at + 1..],
        None => {
            tracing::warn!(
                message_id = through,
                "a compaction names a message the transcript no longer holds"
            );
            messages
        }
    }
}

// ---------------------------------------------------------------------------
// Deriving the state
// ---------------------------------------------------------------------------

/// What was extracted from the folded messages, before it is written out.
#[derive(Debug, Default)]
struct Facts {
    /// The first thing the user asked for.
    goal: Option<String>,
    /// What they asked for afterwards, most recent last.
    asked: Vec<String>,
    /// Paths `fs_write` wrote, in the order first written.
    wrote: Vec<String>,
    /// How many of those writes were to the decisions ledger.
    decisions: usize,
    /// Programs `shell_exec` ran.
    ran: Vec<String>,
    /// Skill runs, and how each ended when it ended.
    skills: Vec<(String, Option<String>)>,
    /// Work handed to other identities, and how the board came back.
    ///
    /// The one fact a Chief of Staff's session cannot afford to fold away: a
    /// CoS transcript is exactly the kind that grows long, and "who is working
    /// on what, and what came of it" is the whole of what it holds.
    handed: Vec<String>,
    /// Questions a run left open.
    blockers: Vec<String>,
    /// Tools whose calls were refused, and how many times.
    refused: BTreeMap<String, usize>,
}

/// Renders the folded messages as state.
fn state(folded: &[Message]) -> String {
    let facts = extract(folded);
    let mut out = format!(
        "Earlier in this session, folded to state. {} messages are no longer in your context. \
         The user still has all of them on screen and every tool call is still in the audit log, \
         so this is a summary of the record, not the record. Do not answer as though you \
         remembered the detail — read the file, or ask.\n",
        folded.len()
    );

    if let Some(goal) = &facts.goal {
        out.push_str(&format!("\nGoal: {goal}"));
    }
    if !facts.asked.is_empty() {
        out.push_str(&format!("\nThen asked: {}", facts.asked.join(" · ")));
    }
    if !facts.wrote.is_empty() {
        out.push_str(&format!("\nFiles written: {}", facts.wrote.join(" · ")));
    }
    if facts.decisions > 0 {
        out.push_str(&format!(
            "\nDecisions: {} filed in decisions/DECISIONS.md — read it rather than recalling them",
            facts.decisions
        ));
    }
    if !facts.ran.is_empty() {
        out.push_str(&format!("\nCommands run: {}", facts.ran.join(" · ")));
    }
    if !facts.skills.is_empty() {
        let runs: Vec<String> = facts
            .skills
            .iter()
            .map(|(name, status)| match status {
                Some(status) => format!("{name} — {status}"),
                None => format!("{name} — no return"),
            })
            .collect();
        out.push_str(&format!("\nSkill runs: {}", runs.join(" · ")));
    }
    if !facts.handed.is_empty() {
        out.push_str("\nHanded out:");
        for handed in &facts.handed {
            out.push_str(&format!("\n- {handed}"));
        }
    }
    if !facts.blockers.is_empty() {
        out.push_str("\nOpen blockers:");
        for blocker in &facts.blockers {
            out.push_str(&format!("\n- {blocker}"));
        }
    }
    if !facts.refused.is_empty() {
        let refused: Vec<String> = facts
            .refused
            .iter()
            .map(|(tool, count)| format!("{tool} ×{count}"))
            .collect();
        out.push_str(&format!(
            "\nRefused earlier: {}. Do not retry a refused call unchanged.",
            refused.join(", ")
        ));
    }

    clamp(out, STATE_MAX_BYTES)
}

/// Walks the folded messages once and collects what they actually did.
fn extract(folded: &[Message]) -> Facts {
    let mut facts = Facts::default();

    for message in folded {
        if message.role == Role::User {
            let line = one_line(
                &message.text,
                if facts.goal.is_none() {
                    GOAL_MAX_CHARS
                } else {
                    ASKED_MAX_CHARS
                },
            );
            if line.is_empty() {
                continue;
            }
            if facts.goal.is_none() {
                facts.goal = Some(line);
            } else {
                facts.asked.push(line);
            }
            continue;
        }

        for call in &message.tool_calls {
            record(&mut facts, call);
        }
    }

    // The most recent requests, not the first ones: what was asked five turns
    // ago and then superseded is exactly what a fold should lose.
    if facts.asked.len() > ASKED_MAX {
        facts.asked.drain(..facts.asked.len() - ASKED_MAX);
    }
    facts.blockers.truncate(BLOCKERS_MAX);
    facts
}

/// Folds one tool call into the facts.
///
/// Only calls that finished `ok` contribute to what was *done*: a write that
/// failed wrote nothing, and a state claiming otherwise would send the model
/// looking for a file that is not there. A refusal contributes to a different
/// line, because being refused is itself a fact worth keeping — it is what
/// stops the model spending the next turn asking for the same thing again.
fn record(facts: &mut Facts, call: &ToolCallRecord) {
    use crate::policy::tool;

    if call.status == ToolCallStatus::Denied {
        *facts.refused.entry(call.tool.clone()).or_default() += 1;
        return;
    }
    if call.status != ToolCallStatus::Ok {
        return;
    }

    let args: Value = serde_json::from_str(&call.args_json).unwrap_or(Value::Null);

    match call.tool.as_str() {
        tool::FS_WRITE => {
            if let Some(path) = args.get("path").and_then(Value::as_str) {
                if DECISIONS_SUFFIXES
                    .iter()
                    .any(|suffix| path.ends_with(suffix))
                {
                    facts.decisions += 1;
                }
                push_unique(&mut facts.wrote, path);
            }
        }
        tool::SHELL_EXEC => {
            if let Some(program) = args.get("program").and_then(Value::as_str) {
                push_unique(&mut facts.ran, program);
            }
        }
        tool::SKILL_RUN => {
            if let Some(name) = args.get("name").and_then(Value::as_str) {
                if facts.skills.len() < LIST_MAX {
                    facts.skills.push((name.to_owned(), None));
                }
            }
        }
        // Who got what, and how the board came back. The owners and goals are
        // the call's own arguments; the outcome is the one-line summary the
        // runtime already put on the record, which is the board's headline.
        // Read from there rather than re-derived, so the fold says exactly what
        // the transcript says.
        tool::HANDOFF_DELEGATE => {
            let briefs = args.get("briefs").and_then(Value::as_array);
            let who: Vec<String> = briefs
                .map(|briefs| {
                    briefs
                        .iter()
                        .filter_map(|brief| {
                            let owner = brief.get("owner").and_then(Value::as_str)?;
                            let goal = brief.get("goal").and_then(Value::as_str)?;
                            Some(format!("{owner}: {}", one_line(goal, ASKED_MAX_CHARS)))
                        })
                        .collect()
                })
                .unwrap_or_default();

            if !who.is_empty() && facts.handed.len() < LIST_MAX {
                let line = match &call.summary {
                    Some(summary) => format!(
                        "{} — {}",
                        who.join(" · "),
                        one_line(summary, ASKED_MAX_CHARS)
                    ),
                    None => who.join(" · "),
                };
                facts.handed.push(line);
            }
        }
        // Both returns are the same object, so both leave their questions in
        // the same place. This arm is reached in a *delegated* session's own
        // transcript — the one the specialist works in — which folds like any
        // other when it runs long.
        tool::SKILL_RETURN | tool::HANDOFF_RETURN => {
            let status = args
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("returned");
            // The run this closes is the last one opened. A return with no run
            // before it is refused by the tool, so it cannot reach a
            // transcript — but a hand-edited one could, and a missing entry is
            // better than a panic.
            if let Some(open) = facts
                .skills
                .iter_mut()
                .rev()
                .find(|(_, status)| status.is_none())
            {
                open.1 = Some(status.to_owned());
            }

            if status == "done" {
                return;
            }
            for question in args
                .get("open_questions")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                if let Some(question) = question.as_str() {
                    let line = one_line(question, BLOCKER_MAX_CHARS);
                    if !line.is_empty() && !facts.blockers.iter().any(|held| held == &line) {
                        facts.blockers.push(line);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Appends `value` if the list does not already hold it and has room.
fn push_unique(list: &mut Vec<String>, value: &str) {
    if list.len() < LIST_MAX && !list.iter().any(|held| held == value) {
        list.push(value.to_owned());
    }
}

/// The first line of `text`, trimmed and capped.
///
/// Multi-line input keeps its first line and is marked as shortened, rather
/// than being joined into one long run-on: the first line of a request is
/// almost always the request, and the rest is almost always the detail that
/// compaction is *for* dropping.
fn one_line(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let first = trimmed.lines().next().unwrap_or("").trim();
    let more = trimmed.lines().nth(1).is_some();

    let mut out: String = first.chars().take(max).collect();
    if first.chars().count() > max || more {
        out.push('…');
    }
    out
}

/// Truncates on a character boundary, and says that it did.
fn clamp(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…(state truncated)", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::policy::tool;
    use crate::store::ToolCallStatus;

    fn call(name: &str, args: &str, status: ToolCallStatus) -> ToolCallRecord {
        ToolCallRecord {
            call_id: format!("c-{name}-{}", args.len()),
            tool: name.to_owned(),
            args_json: args.to_owned(),
            status,
            summary: None,
            image_path: None,
        }
    }

    /// Four turns of chatter, then a fifth: the shape every test here needs.
    fn conversation(turns: usize) -> Vec<Message> {
        let mut messages = Vec::new();
        for n in 0..turns {
            messages.push(Message::user(format!("request {n}")));
            messages.push(Message::assistant(format!("reply {n}"), Vec::new()));
        }
        messages
    }

    #[test]
    fn nothing_folds_until_there_are_more_turns_than_are_kept() {
        for turns in 0..=KEEP_TURNS {
            let messages = conversation(turns);
            assert!(
                cut(&messages, KEEP_TURNS).is_none(),
                "{turns} turns is not enough to fold"
            );
            assert!(plan(&messages, true).is_none(), "not even when forced");
        }
    }

    /// The invariant the request's validity rests on.
    #[test]
    fn the_cut_always_lands_on_a_user_message() {
        let mut messages = conversation(6);
        messages.insert(3, Message::tool("call_1", "{}"));

        let at = cut(&messages, KEEP_TURNS).expect("something to fold");
        assert_eq!(messages[at].role, Role::User, "the first kept message");
        assert_ne!(at, 0);
    }

    #[test]
    fn folding_keeps_the_last_turns_raw_and_the_rest_becomes_state() {
        let messages = conversation(6);
        let plan = plan(&messages, true).expect("something to fold");

        let kept = tail(&messages, Some(&plan.through_message_id));
        assert_eq!(kept.len(), KEEP_TURNS * 2);
        assert_eq!(kept[0].text, "request 2", "four turns kept");
        assert_eq!(plan.folded, 4, "two turns folded");

        assert!(plan.state.contains("Goal: request 0"), "{}", plan.state);
        assert!(
            plan.state.contains("Then asked: request 1"),
            "{}",
            plan.state
        );
    }

    /// The size threshold is what makes it automatic; the button is what makes
    /// it forced. They fold the same amount.
    #[test]
    fn an_unforced_fold_waits_for_the_transcript_to_get_expensive() {
        let mut messages = conversation(6);
        assert!(plan(&messages, false).is_none(), "still cheap");

        messages.push(Message::user("x".repeat(COMPACT_AT_BYTES)));
        messages.push(Message::assistant("ok", Vec::new()));
        let heavy = plan(&messages, false).expect("expensive enough now");
        let forced = plan(&messages, true).expect("and forced folds the same");
        assert_eq!(heavy.through_message_id, forced.through_message_id);
    }

    #[test]
    fn the_state_names_what_was_done_and_not_what_was_attempted() {
        let mut messages = conversation(6);
        messages.insert(
            1,
            Message::assistant(
                "",
                vec![
                    call(
                        tool::FS_WRITE,
                        r#"{"path":"artefacts/plan.md","content":"x"}"#,
                        ToolCallStatus::Ok,
                    ),
                    call(
                        tool::FS_WRITE,
                        r#"{"path":"decisions/DECISIONS.md","content":"y"}"#,
                        ToolCallStatus::Ok,
                    ),
                    call(
                        tool::FS_WRITE,
                        r#"{"path":"never/written.md","content":"z"}"#,
                        ToolCallStatus::Error,
                    ),
                    call(
                        tool::SHELL_EXEC,
                        r#"{"program":"cargo"}"#,
                        ToolCallStatus::Ok,
                    ),
                    call(
                        tool::SHELL_EXEC,
                        r#"{"program":"rm"}"#,
                        ToolCallStatus::Denied,
                    ),
                ],
            ),
        );

        let state = plan(&messages, true).expect("something to fold").state;

        assert!(state.contains("artefacts/plan.md"), "{state}");
        assert!(
            !state.contains("never/written.md"),
            "a failed write wrote nothing: {state}"
        );
        assert!(state.contains("decisions/DECISIONS.md"), "{state}");
        assert!(state.contains("Commands run: cargo"), "{state}");
        assert!(
            state.contains("shell_exec ×1"),
            "a refusal is state too: {state}"
        );
    }

    #[test]
    fn a_skill_run_keeps_its_status_and_its_open_questions() {
        let mut messages = conversation(6);
        messages.insert(
            1,
            Message::assistant(
                "",
                vec![
                    call(
                        tool::SKILL_RUN,
                        r#"{"name":"inbox.triage"}"#,
                        ToolCallStatus::Ok,
                    ),
                    call(
                        tool::SKILL_RETURN,
                        r#"{"status":"blocked","summary":"no source","open_questions":["which mailbox"]}"#,
                        ToolCallStatus::Ok,
                    ),
                ],
            ),
        );

        let state = plan(&messages, true).expect("something to fold").state;
        assert!(state.contains("inbox.triage — blocked"), "{state}");
        assert!(state.contains("- which mailbox"), "{state}");
    }

    /// What a Chief of Staff's session must not fold away: who is working on
    /// what, and how the board came back. The outcome is read off the record
    /// the runtime already wrote, so the state says what the transcript says.
    #[test]
    fn a_delegation_keeps_its_owners_and_what_the_board_said() {
        let mut messages = conversation(6);
        let mut handed = call(
            tool::HANDOFF_DELEGATE,
            r#"{"briefs":[{"goal":"Draft the release note","owner":"Scribe"},
                          {"goal":"Check the changelog","owner":"Reader"}]}"#,
            ToolCallStatus::Ok,
        );
        handed.summary = Some("2 briefs: 1 done, 1 blocked".to_owned());
        messages.insert(1, Message::assistant("", vec![handed]));

        let state = plan(&messages, true).expect("something to fold").state;

        assert!(state.contains("Handed out:"), "{state}");
        assert!(state.contains("Scribe: Draft the release note"), "{state}");
        assert!(state.contains("Reader: Check the changelog"), "{state}");
        assert!(state.contains("1 done, 1 blocked"), "{state}");
    }

    /// A brief a specialist could not finish leaves its question behind, in
    /// that specialist's own session — which folds like any other.
    #[test]
    fn a_returned_brief_leaves_its_question_behind() {
        let mut messages = conversation(6);
        messages.insert(
            1,
            Message::assistant(
                "",
                vec![call(
                    tool::HANDOFF_RETURN,
                    r#"{"status":"blocked","summary":"no source","open_questions":["which changelog"]}"#,
                    ToolCallStatus::Ok,
                )],
            ),
        );

        let state = plan(&messages, true).expect("something to fold").state;
        assert!(state.contains("- which changelog"), "{state}");
    }

    /// A `done` run has nothing open, so it contributes no blocker.
    #[test]
    fn a_finished_run_leaves_no_blocker_behind() {
        let mut messages = conversation(6);
        messages.insert(
            1,
            Message::assistant(
                "",
                vec![
                    call(
                        tool::SKILL_RUN,
                        r#"{"name":"watch.digest"}"#,
                        ToolCallStatus::Ok,
                    ),
                    call(
                        tool::SKILL_RETURN,
                        r#"{"status":"done","summary":"filed","open_questions":["stale"]}"#,
                        ToolCallStatus::Ok,
                    ),
                ],
            ),
        );

        let state = plan(&messages, true).expect("something to fold").state;
        assert!(state.contains("watch.digest — done"), "{state}");
        assert!(!state.contains("Open blockers"), "{state}");
    }

    #[test]
    fn the_state_stays_small_however_long_the_session_was() {
        let mut messages = Vec::new();
        for n in 0..200 {
            messages.push(Message::user(format!("request {n} ").repeat(40)));
            messages.push(Message::assistant(
                "",
                vec![call(
                    tool::FS_WRITE,
                    &format!(r#"{{"path":"artefacts/{n}.md","content":"x"}}"#),
                    ToolCallStatus::Ok,
                )],
            ));
        }

        let state = plan(&messages, true).expect("something to fold").state;
        assert!(state.len() <= STATE_MAX_BYTES + 32, "{} bytes", state.len());
    }

    /// A stale pointer must not blank a session. Too much context is
    /// recoverable; none is not.
    #[test]
    fn a_pointer_at_a_message_that_is_gone_keeps_the_whole_transcript() {
        let messages = conversation(3);
        assert_eq!(tail(&messages, Some("no-such-id")).len(), messages.len());
        assert_eq!(tail(&messages, None).len(), messages.len());
    }

    #[test]
    fn a_multi_line_request_contributes_its_first_line_and_says_so() {
        let messages = {
            let mut messages = conversation(6);
            messages[0] = Message::user("ship the release\nand also do everything else");
            messages
        };

        let state = plan(&messages, true).expect("something to fold").state;
        assert!(state.contains("Goal: ship the release…"), "{state}");
        assert!(!state.contains("everything else"), "{state}");
    }
}
