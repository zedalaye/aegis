//! `memory_write` and `memory_search` (PLAN 7.3, Phase 14).
//!
//! Role memory as two verbs: **remember this** and **what do I know about
//! that**. Both are scoped to the identity the turn is running as, by the
//! runtime and not by the arguments — there is no field on either call that
//! names an identity, so an identity reading or writing another's memories is
//! not a thing these signatures can express.
//!
//! ## There is no third verb
//!
//! Deliberately no `memory_forget`. `COS.md` *Roles* gives the human three
//! jobs — irreversible decisions, the quality bar, and **memory correction** —
//! and deleting a memory is the first and third at once: it is irreversible,
//! and what it most often deletes is a correction a person made. So forgetting
//! lives in the Memory panel ([`commands::memory`](crate::commands::memory)),
//! where the person who owns the identity does it, sees what went, and can
//! retype it.
//!
//! What the model gets instead is [`memories::prompt_block`] telling it plainly
//! that it cannot delete these and should say so when one is wrong. That is the
//! honest division: the agent notices, the human corrects. An agent that could
//! quietly retire the memories it found inconvenient would be an agent whose
//! memory is exactly as reliable as its judgement on its worst turn.
//!
//! ## Why a write is asked about
//!
//! `memory_write` goes through the approval gate like `fs_write`, and for the
//! same reason: it is durable and it changes what happens later. A memory
//! reaches the system message of *every* future turn this identity takes, which
//! makes it closer to an instruction than to a note. The dialog shows the exact
//! sentence that would be remembered, which is a great deal less to read than a
//! diff, and `allow_session` is offered because "let it remember things while
//! we work" is a scope a person can picture and a session can end.
//!
//! `memory_search` is auto: it reads records this identity already holds, it
//! reaches nothing outside the process, and there is no version of it a user
//! could usefully be asked about.

use serde_json::{json, Value};

use crate::error::ErrorCode;
use crate::policy::tool;
use crate::store::memories::{self, MemoryKind, MemoryStore};
use crate::store::MemoryDraft;

use super::Produced;

/// JSON Schema for `memory_write` arguments.
pub fn write_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["preference", "exception", "convention"],
                "description": "preference — how someone likes things done; exception — where \
                                the usual rule does not apply; convention — how it is done here. \
                                Anything that is none of these is not a memory: a fact about the \
                                project is a file in the workspace, and a procedure is a skill.",
            },
            "text": {
                "type": "string",
                "description": "One sentence, at most 280 characters. Write it so it still makes \
                                sense months from now, with no conversation around it.",
            },
            "source": {
                "type": "string",
                "description": "What this rests on — a workspace path, a ticket, or the person \
                                who said it. Optional, and its absence is shown: a memory with \
                                no source is read as a hypothesis.",
            },
        },
        "required": ["kind", "text"],
        "additionalProperties": false,
    })
}

/// JSON Schema for `memory_search` arguments.
pub fn search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Words that must all appear in the memory or its source. Empty \
                                lists what is held. Matching is plain substring, so prefer the \
                                words someone would actually have written.",
            },
        },
        "required": [],
        "additionalProperties": false,
    })
}

/// Records a memory for the identity this turn is running as.
///
/// The store consolidates: a write repeating something already held touches
/// that record instead of storing a second copy, and the result says which
/// happened. Saying so matters — a model told "recorded" twice would have no
/// way to notice it is repeating itself, and one told "you already knew this"
/// can stop.
pub(crate) fn write(
    store: &MemoryStore,
    agent_id: &str,
    kind: MemoryKind,
    text: &str,
    source: Option<&str>,
) -> Produced {
    let held_before = store.count_for(agent_id);
    let draft = MemoryDraft {
        kind,
        text: text.to_owned(),
        source: source.map(str::to_owned),
    };

    match store.save(agent_id, None, &draft) {
        Ok(memory) => {
            let fresh = store.count_for(agent_id) > held_before;
            let content = format!(
                "{} {}\n\nIt is in your instructions from the next turn on. You cannot delete it \
                 — if it turns out to be wrong, say so and the user will correct it.",
                if fresh {
                    "Remembered:"
                } else {
                    "Already held, and confirmed:"
                },
                memory.line(),
            );
            let bytes = memory.text.len() as u64;

            Produced::ok(
                tool::MEMORY_WRITE,
                if fresh {
                    format!("remembered ({})", memory.kind.as_str())
                } else {
                    format!("already held ({})", memory.kind.as_str())
                },
                content,
                bytes,
                false,
                json!({
                    "id": memory.id,
                    "kind": memory.kind.as_str(),
                    "new": fresh,
                    "held": store.count_for(agent_id),
                }),
            )
            .with_bytes_in(bytes)
        }
        // The caps and the empty check. `ToolFailed` rather than `Denied`:
        // nothing was refused on grounds of rights, the value simply cannot be
        // stored, and the message says what a storable one looks like.
        Err(err) => Produced::failed(tool::MEMORY_WRITE, ErrorCode::ToolFailed, err.to_string()),
    }
}

/// Finds what this identity remembers.
///
/// A miss is a success with nothing in it, not a failure: "I have nothing on
/// that" is an answer, and an error envelope would invite the model to explain
/// a fault that did not happen.
pub(crate) fn search(store: &MemoryStore, agent_id: &str, query: &str) -> Produced {
    let found = store.search(agent_id, query);
    let held = store.count_for(agent_id);

    let content = if found.is_empty() {
        format!(
            "Nothing remembered matches `{query}`. {}",
            if held == 0 {
                "This identity holds no memories at all.".to_owned()
            } else {
                format!("It holds {held}, none of them about this.")
            }
        )
    } else {
        let lines: Vec<String> = found.iter().map(memories::Memory::line).collect();
        format!(
            "{} of {held} remembered {} `{query}`:\n\n{}",
            found.len(),
            if found.len() == 1 { "matches" } else { "match" },
            lines.join("\n")
        )
    };

    let bytes = content.len() as u64;
    Produced::ok(
        tool::MEMORY_SEARCH,
        match found.len() {
            0 => "no memory matches".to_owned(),
            1 => "1 memory".to_owned(),
            n => format!("{n} memories"),
        },
        content,
        bytes,
        // The store caps the result; a caller that hit the cap is being told
        // less than there is.
        found.len() >= memories::SEARCH_MAX_RESULTS,
        json!({ "matched": found.len(), "held": held }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn store() -> (TempDir, MemoryStore) {
        let dir = TempDir::new().expect("temp dir");
        let store = MemoryStore::load(dir.path());
        (dir, store)
    }

    #[test]
    fn a_write_records_it_and_says_it_cannot_be_taken_back() {
        let (_dir, store) = store();

        let produced = write(
            &store,
            "a",
            MemoryKind::Preference,
            "this client wants French",
            Some(".aegis/briefs/client.md"),
        );

        assert!(produced.result.ok, "{:?}", produced.result.error);
        assert!(produced.result.content.contains("Remembered:"));
        assert!(produced.result.content.contains(".aegis/briefs/client.md"));
        assert!(
            produced.result.content.contains("cannot delete it"),
            "the model is told who corrects a memory: {}",
            produced.result.content
        );
        assert_eq!(produced.result.meta["new"], json!(true));
        assert_eq!(store.count_for("a"), 1);
    }

    /// A model repeating itself should be able to tell that it is.
    #[test]
    fn a_repeat_is_a_success_that_says_it_was_already_held() {
        let (_dir, store) = store();
        write(
            &store,
            "a",
            MemoryKind::Preference,
            "answers in French",
            None,
        );

        let again = write(
            &store,
            "a",
            MemoryKind::Preference,
            "answers in French",
            None,
        );

        assert!(again.result.ok);
        assert_eq!(again.result.meta["new"], json!(false));
        assert!(again.result.content.contains("Already held"));
        assert_eq!(store.count_for("a"), 1);
    }

    #[test]
    fn a_memory_that_cannot_be_stored_is_refused_in_words_the_model_can_act_on() {
        let (_dir, store) = store();

        let produced = write(&store, "a", MemoryKind::Convention, "   ", None);

        assert!(!produced.result.ok);
        assert_eq!(
            produced.result.error.as_ref().map(|error| error.code),
            Some(ErrorCode::ToolFailed)
        );
        assert!(
            produced
                .result
                .error
                .expect("an error")
                .message
                .contains("one sentence"),
            "the refusal says what a storable memory looks like"
        );
    }

    #[test]
    fn a_search_only_ever_sees_this_identitys_memories() {
        let (_dir, store) = store();
        write(
            &store,
            "a",
            MemoryKind::Preference,
            "answers in French",
            None,
        );
        write(
            &store,
            "b",
            MemoryKind::Preference,
            "answers in Dutch",
            None,
        );

        let mine = search(&store, "a", "answers");
        assert!(mine.result.ok);
        assert!(mine.result.content.contains("French"));
        assert!(
            !mine.result.content.contains("Dutch"),
            "another identity's memory is not reachable: {}",
            mine.result.content
        );
        assert_eq!(mine.result.meta["held"], json!(1));
    }

    #[test]
    fn a_search_that_finds_nothing_is_an_answer_and_not_a_failure() {
        let (_dir, store) = store();
        write(
            &store,
            "a",
            MemoryKind::Preference,
            "answers in French",
            None,
        );

        let produced = search(&store, "a", "deployment");

        assert!(produced.result.ok, "a miss is not an error");
        assert!(produced.result.content.contains("none of them about this"));
        assert_eq!(produced.result.meta["matched"], json!(0));
    }
}
