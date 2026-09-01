//! The skill runner (PLAN 7.3, Phase 13; PLAN 7.6; `COS.md` *Skills*).
//!
//! A skill is a versioned `SKILL.md` runbook — not a memory, not a tool. A
//! tool is a verb on the machine. A memory is a preference or an exception. A
//! skill *sequences* tools toward a done criterion, under the identity's
//! allow-list, and it is the layer that makes a Chef de Cabinet cheap instead
//! of chatty: without it every specialist re-derives "how we triage mail" from
//! a novel in the context window, tokens burn, the process drifts, and
//! compaction wipes it.
//!
//! ## Catalog in, body on demand
//!
//! This is the shape the whole phase turns on, and the reason for every seam
//! below. Every turn's system message carries the **catalog**: one line per
//! skill the identity may run — name, version, scope, when to use it, the
//! tools it will call ([`prompt_block`]). The **body** is not there. It is
//! read, parsed and handed over only when the model calls `skill_run`, into
//! that turn, and the next turn does not carry it unless it runs the skill
//! again. Stuffing every `SKILL.md` into every system prompt is precisely the
//! anti-pattern the catalog exists to prevent (PLAN 7.6), so [`Skill`] does
//! not hold a body at all: [`catalog`] keeps the line, [`load`] re-reads the
//! one file that was actually invoked. The rule is a property of the types,
//! not a habit of the caller.
//!
//! ## Three scopes, two directories, one allow-list
//!
//! `COS.md` names three scopes. Two of them are places on disk — the user's
//! **library**, beside the other application data, and the **workspace**'s own
//! `skills/`, which is where "how *this* project is deployed" belongs and
//! which travels with the folder in git. The third, per-agent, is not a
//! directory: it is [`Agent::skills`](crate::store::Agent::skills), the
//! identity's allow-list, and it selects from what the other two found
//! ([`granted`]). A workspace skill shadows a library skill of the same name,
//! because the more specific runbook is the one that knows about the project.
//!
//! ## No extra rights
//!
//! A skill never widens the tool allow-list (PLAN 7.3, Phase 13). Two things
//! enforce that and neither is inside the runbook:
//!
//! * the identity's *skill* allow-list is checked by
//!   [`policy::decide_call`](crate::policy::decide_call), before anything is
//!   read, with no approval offered — a prompt to exceed an allow-list is not
//!   a question to ask; and
//! * a run whose declared `tools` are not all held by the identity **fails
//!   closed** at `skill_run`, rather than halfway through the steps.
//!
//! Everything a skill's steps then do is an ordinary tool call through the
//! ordinary gate: the same matrix, the same dialog, the same audit line — now
//! with the skill's name on it, which is what makes a run budgetable and
//! replayable later (PLAN 7.6, *Audit names the skill*).

pub mod doc;

use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;
use ts_rs::TS;

use crate::store::Agent;
use crate::tools::ToolResult;

pub use doc::SkillDoc;

/// The directory skills live in — in the library and in a workspace alike.
///
/// One name for both, so "where do skills go" has one answer whichever scope
/// someone is writing for.
pub const LIBRARY_DIR: &str = "skills";

/// The runbook inside a skill's directory.
///
/// The directory is the skill's *name* and the file is always called this, as
/// `COS.md` and PLAN 7.3 both write it. A directory rather than a bare
/// `<name>.md` because a runbook grows attachments — a template, an example,
/// a checklist — and they belong beside it rather than in a second tree.
pub const SKILL_FILE: &str = "SKILL.md";

/// Longest skill name.
pub const NAME_MAX_CHARS: usize = 64;

/// Most skills one catalog holds.
///
/// A ceiling on what the system message can cost, since the catalog is in
/// every request. Past it the library is not a library, it is a wiki, and the
/// answer is per-agent allow-lists rather than a longer prompt.
pub const CATALOG_MAX: usize = 64;

// ---------------------------------------------------------------------------
// IPC payloads
// ---------------------------------------------------------------------------

/// Which of `COS.md`'s scopes a skill was found in.
///
/// The per-agent scope is not here because it is not a place: it is the
/// identity's allow-list, applied to what these two found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum SkillScope {
    /// The user's own library, beside the other application data.
    Library,
    /// The open project's folder, where it travels with the repository.
    Workspace,
}

impl SkillScope {
    /// How the scope is named to the model, in the catalog line.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Workspace => "this workspace",
        }
    }
}

/// One catalog entry.
///
/// Everything except the runbook. A `Skill` is what the panel draws, what the
/// system message is built from, and what the allow-list is matched against;
/// the body is [`load`]ed only by a run, which is the whole point of the type
/// not having a field for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Skill {
    /// The skill's name, which is its directory's: `inbox.triage`.
    pub name: String,
    /// Where it was found.
    pub scope: SkillScope,
    /// What the author versioned it as. Empty when the file will not parse.
    pub version: String,
    /// The first paragraph of *When to use it*, capped. Empty when it will not
    /// parse.
    pub summary: String,
    /// The tools its steps declare they will call.
    pub tools: Vec<String>,
    /// The `SKILL.md` itself, so a person can go and open it.
    pub path: String,
    /// Whether a workspace skill of this name is hiding a library one.
    ///
    /// Reported rather than silently resolved: two runbooks with one name is
    /// exactly the state where somebody is running the one they did not mean
    /// to, and the panel can say so.
    pub shadows: bool,
    /// Why this one cannot run, when it cannot.
    ///
    /// A file that will not parse stays in the catalog carrying its refusal,
    /// rather than disappearing: the author is the only person who can fix it,
    /// and a skill that vanished would tell them nothing. It is never offered
    /// to the model — [`granted`] drops it.
    pub problem: Option<String>,
}

impl Skill {
    /// Whether this entry can actually be run.
    pub const fn runnable(&self) -> bool {
        self.problem.is_none()
    }
}

/// Whether `name` is a skill name.
///
/// The one definition, shared by discovery and by the identity allow-list
/// ([`store::agents`](crate::store::agents)), so a name that can be granted is
/// a name a directory can have and there is no third spelling in between.
/// Lower case because the name reaches a case-insensitive filesystem on two of
/// the three platforms, and a library that behaved differently on Linux would
/// be a library that breaks when it is shared.
pub fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= NAME_MAX_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Every skill the library and this workspace hold, by name.
///
/// Never fails. A library directory that is not there, or not readable, is a
/// catalog without it — this is called on the way into a turn, and a turn that
/// would not start because a folder is missing is a session that can no longer
/// be talked to.
///
/// Workspace skills are discovered second and shadow library ones of the same
/// name. The one they hide is dropped rather than listed twice: the model
/// resolves a name to one runbook, and a catalog offering two of them would be
/// offering a choice nothing downstream can express.
pub fn catalog(library: &Path, workspace: Option<&Path>) -> Vec<Skill> {
    let mut found = read_dir(library, SkillScope::Library);

    if let Some(root) = workspace {
        for mut skill in read_dir(&root.join(LIBRARY_DIR), SkillScope::Workspace) {
            if let Some(at) = found.iter().position(|other| other.name == skill.name) {
                tracing::debug!(
                    skill = %skill.name,
                    "a workspace runbook shadows one in the library"
                );
                found.remove(at);
                skill.shadows = true;
            }
            found.push(skill);
        }
    }

    found.sort_by(|a, b| a.name.cmp(&b.name));
    if found.len() > CATALOG_MAX {
        tracing::warn!(
            found = found.len(),
            kept = CATALOG_MAX,
            "more skills than a catalog carries; narrow them with an identity's allow-list"
        );
        found.truncate(CATALOG_MAX);
    }
    found
}

/// One scope's directory, read into catalog entries.
fn read_dir(root: &Path, scope: SkillScope) -> Vec<Skill> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::debug!(%err, dir = %root.display(), "no skills directory to read");
            return Vec::new();
        }
    };

    let mut found = Vec::new();

    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();

        // A folder that is not named like a skill is somebody else's folder,
        // not a broken skill: `skills/` is an ordinary directory in an
        // ordinary workspace and may well have a `.git` or a `README.md` in
        // it. Only a directory holding a `SKILL.md` claims to be one.
        if !is_name(&name) {
            continue;
        }
        let path = entry.path().join(SKILL_FILE);
        if !path.is_file() {
            continue;
        }

        found.push(entry_for(&name, scope, &path));
    }

    found
}

/// One `SKILL.md` as a catalog entry, whether or not it parses.
fn entry_for(name: &str, scope: SkillScope, path: &Path) -> Skill {
    let mut skill = Skill {
        name: name.to_owned(),
        scope,
        version: String::new(),
        summary: String::new(),
        tools: Vec::new(),
        path: path.display().to_string(),
        shadows: false,
        problem: None,
    };

    match read(path) {
        Ok(doc) => {
            skill.version = doc.version;
            skill.summary = doc.summary;
            skill.tools = doc.tools;
        }
        Err(problem) => {
            tracing::info!(skill = name, %problem, "a runbook will not run as written");
            skill.problem = Some(problem);
        }
    }

    skill
}

/// Reads and parses one `SKILL.md`.
///
/// The `io` failure is folded into the same message as a parse failure,
/// because both answer the one question the caller has — can this run, and if
/// not, what does the author have to change.
fn read(path: &Path) -> Result<SkillDoc, String> {
    let bytes = fs::read(path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => format!("there is no `{SKILL_FILE}` in this folder"),
        io::ErrorKind::PermissionDenied => "this file is not readable".to_owned(),
        _ => format!("this file could not be read: {err}"),
    })?;

    let text =
        String::from_utf8(bytes).map_err(|_| format!("`{SKILL_FILE}` has to be UTF-8 text"))?;

    doc::parse(&text)
}

/// The runbook of one catalog entry, read now.
///
/// Separate from [`catalog`] and re-reading the file on purpose: this is the
/// *body on demand* half of PLAN 7.6, and a catalog that had already loaded
/// every body would make the rule a habit of the caller rather than a fact
/// about the code. It also means a runbook edited between two turns is the one
/// that runs.
pub fn load(skill: &Skill) -> Result<SkillDoc, String> {
    read(Path::new(&skill.path))
}

/// The entry a name resolves to.
pub fn find<'a>(catalog: &'a [Skill], name: &str) -> Option<&'a Skill> {
    catalog.iter().find(|skill| skill.name == name)
}

// ---------------------------------------------------------------------------
// The allow-list, and the catalog the model sees
// ---------------------------------------------------------------------------

/// The skills `agent` may run, in catalog order.
///
/// Two filters, and both matter. An identity is shown only what it was granted
/// — the per-agent scope of `COS.md` — and a runbook that will not parse is
/// never offered, because offering it would be offering a run that cannot
/// start. The refusal for the second stays in the catalog for the panel, where
/// the person who can fix it will see it.
pub fn granted<'a>(catalog: &'a [Skill], agent: &Agent) -> Vec<&'a Skill> {
    catalog
        .iter()
        .filter(|skill| skill.runnable() && agent.allows_skill(&skill.name))
        .collect()
}

/// The catalog block for the system message, or `None` when there is none.
///
/// `None` rather than a line saying "you have no skills": an identity that was
/// granted none is every identity from before this phase, and a prompt that
/// grew a paragraph about a feature it does not use would be the system
/// message drifting the way PLAN 7.1 says it must not.
///
/// What each line carries is what a decision to load one is made on — the
/// name, when it applies, and what it will touch. Not the steps: they are the
/// body, and the body arrives when it is invoked.
pub fn prompt_block(skills: &[&Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut block = String::from(
        "Skills you may run. A skill is a runbook someone already wrote down: call `skill_run` \
         with its name and the steps are loaded into this turn, then follow them and finish with \
         `skill_return`. The steps are not in this message and they do not carry over to the \
         next turn, so load one when you are about to follow it. Running a skill grants nothing \
         — every step is an ordinary tool call through the usual approval gate.\n",
    );

    for skill in skills {
        block.push_str(&format!(
            "\n- `{}` (v{}, {}) — {}",
            skill.name,
            skill.version,
            skill.scope.as_str(),
            if skill.summary.is_empty() {
                "no description"
            } else {
                &skill.summary
            }
        ));
        if !skill.tools.is_empty() {
            block.push_str(&format!(" Calls {}.", skill.tools.join(", ")));
        }
    }

    Some(block)
}

// ---------------------------------------------------------------------------
// The run, as the turn sees it
// ---------------------------------------------------------------------------

/// What a skill tool needs from the runtime around it.
///
/// Held by [`ToolCtx`](crate::tools::ToolCtx) rather than resolved inside the
/// tool, for the reason the capture directory is: where the library lives and
/// which identity is running are facts about the installation and the session,
/// not about what the model asked for, and a tool that went looking for them
/// itself could not be tested without an application.
#[derive(Debug, Clone, Copy)]
pub struct SkillCtx<'a> {
    /// The user's library directory.
    pub library: &'a Path,
    /// The session's workspace root, when it has one.
    pub workspace: Option<&'a Path>,
    /// The tools the identity holds, for the fail-closed check.
    pub tools: &'a [String],
    /// The skill this turn is currently running, if any.
    ///
    /// Set by [`track`] when `skill_run` succeeds and cleared when
    /// `skill_return` does. Every audit line written while it is set carries
    /// the name, which is what makes a run budgetable and replayable
    /// (PLAN 7.6, *Audit names the skill*).
    pub active: Option<&'a str>,
}

impl SkillCtx<'_> {
    /// Whether the identity holds every tool a runbook says it will call.
    ///
    /// Returns the first one it does not, which is what the refusal names.
    pub fn missing<'d>(&self, declared: &'d [String]) -> Option<&'d str> {
        declared
            .iter()
            .find(|wanted| !self.tools.iter().any(|held| held == *wanted))
            .map(String::as_str)
    }
}

/// The name a skill tool reports it opened, in [`ToolResult::meta`].
///
/// The turn reads the run out of the envelope rather than out of the
/// arguments: what opened is what the tool decided, and a loop that re-derived
/// it from the call would be a second copy of that decision.
pub const META_SKILL: &str = "skill";

/// The status a `skill_return` recorded, in [`ToolResult::meta`].
pub const META_STATUS: &str = "status";

/// The summary a `skill_return` recorded, in [`ToolResult::meta`].
pub const META_SUMMARY: &str = "summary";

/// What a run reported, for whoever started the turn (PLAN 7.3, Phase 16).
///
/// The three fields of a `skill_return` anybody outside the turn has any use
/// for: which runbook, how it ended, and what it said. A scheduled run's row is
/// built from this, and nothing else in the process reads it — a session
/// somebody is watching reports by being watched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Returned {
    /// The runbook that returned.
    pub skill: String,
    /// `done`, `blocked` or `needs_you`.
    pub status: String,
    /// The summary, as the return carried it.
    pub summary: String,
}

/// Where a turn leaves what its last run returned.
///
/// A cell, and the same one Phase 15 wrote for a delegated run
/// ([`handoff::Open`](crate::handoff::Open)): the caller creates it, lends it to
/// the turn, and reads it once the turn is over. Nothing here survives the
/// turn, which is the scope a run has anyway.
///
/// The mutex is not contention — a turn runs its tool calls one at a time — it
/// is there because the cell is reached through a shared reference from inside
/// the loop.
#[derive(Debug, Default)]
pub struct Reported {
    returned: std::sync::Mutex<Option<Returned>>,
}

impl Reported {
    /// A cell with nothing in it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a return. The last one wins, which is a model correcting itself.
    pub fn close(&self, returned: Returned) {
        *self.lock() = Some(returned);
    }

    /// Takes what was returned, leaving the cell empty.
    pub fn take(&self) -> Option<Returned> {
        self.lock().take()
    }

    /// Locks the cell, recovering from a poisoned mutex.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Returned>> {
        self.returned
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Reads a tool result as the return it recorded, if it is one.
///
/// Out of the envelope rather than out of the arguments, for the reason
/// [`track`] is: what was recorded is what the tool decided, and a caller that
/// re-derived it from the call would be a second copy of that decision — one
/// that would happily report a return the tool had refused.
pub fn returned(tool: &str, result: &ToolResult) -> Option<Returned> {
    if tool != crate::policy::tool::SKILL_RETURN || !result.ok {
        return None;
    }

    let text = |key: &str| {
        result
            .meta
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };

    Some(Returned {
        skill: text(META_SKILL),
        status: text(META_STATUS),
        summary: text(META_SUMMARY),
    })
}

/// Follows a turn's skill run across the tool calls of one round.
///
/// A run is a span *within a turn* and not a session-long state machine. That
/// is the same scope the body has — loaded into this turn, gone from the next
/// — and it keeps the fact in one local variable that cannot be left set by a
/// crash, a cancel or a window closing. A turn that ends mid-run is a run that
/// did not return, which the loop says out loud rather than carrying into a
/// conversation that has moved on.
pub fn track(active: &mut Option<String>, tool: &str, result: &ToolResult) {
    if !result.ok {
        return;
    }

    if tool == crate::policy::tool::SKILL_RUN {
        *active = result
            .meta
            .get(META_SKILL)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
    } else if tool == crate::policy::tool::SKILL_RETURN {
        *active = None;
    }
}

// ---------------------------------------------------------------------------
// The library on disk
// ---------------------------------------------------------------------------

/// Creates the library and puts the example runbooks in it, on a first run
/// only.
///
/// Keyed on the *directory* not existing rather than on the file: seeding a
/// skill someone deleted, every time the app starts, would be an application
/// arguing with its user about the contents of their own folder. Once the
/// directory is there the library is theirs.
///
/// Best effort. A library that could not be created costs the examples and
/// nothing else — [`catalog`] reads a missing directory as an empty one.
///
/// Two runbooks, and they are the two halves of the mode: the standing rule
/// that nothing irreversible goes out unreviewed, and the loop a Chief of Staff
/// runs. Neither is granted to anything by being here — an identity that may
/// run one is an identity somebody granted it to (PLAN 7.6, *Authoring*).
pub fn seed(library: &Path) {
    if library.exists() {
        return;
    }

    for (name, body) in [(REVIEW_SKILL, REVIEW_SEED), (COS_SKILL, COS_SEED)] {
        let dir = library.join(name);
        if let Err(err) = fs::create_dir_all(&dir) {
            tracing::warn!(%err, dir = %dir.display(), "could not create the skill library");
            return;
        }
        if let Err(err) = fs::write(dir.join(SKILL_FILE), body) {
            tracing::warn!(%err, name, "could not write an example skill");
            return;
        }
    }

    tracing::info!(dir = %library.display(), "skill library created with two examples");
}

/// One of the two examples the library starts with.
pub const REVIEW_SKILL: &str = "never-send-without-review";

/// `never-send-without-review`, the one skill a fresh library holds.
///
/// Chosen because it is the standing rule of the whole mode rather than a
/// domain: irreversible actions stay behind a human gate (`COS.md` *Loop*),
/// and the skill that checks a draft against its own definition of done is the
/// most valuable one there is (PLAN 7.6, *Verifier is a skill*). It also
/// demonstrates the format on something that needs no connector — the input is
/// a file, which is all a skill ever needs to start.
const REVIEW_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write
---

# never-send-without-review

## When to use it

Before anything leaves this machine: an email, a message, a post, a reply, a
commit somebody else will read. Run it on a draft that exists as a file, not
on an idea in the conversation.

## Inputs required and tools it will call

- The draft, as a path in the workspace. A path, never pasted text.
- Who it goes to and what it is meant to achieve — one line each.

Calls `fs_read` to read the draft and whatever it cites, and `fs_write` to put
the review beside it.

## Steps

1. `fs_read` the draft in full. If it cites a file, a ticket or a number, read
   that too rather than trusting the draft's account of it.
2. Check four things, in this order: is every factual claim supported by
   something you read; does it do what it was meant to do; is anything in it
   irreversible once sent; and is there anything in it the recipient should
   not see.
3. Write the review to `artefacts/<draft name>.review.md`. Say what you
   checked, quote the lines you would change, and end with one of *send*,
   *change first* or *do not send*.
4. Stop. Sending is not a step, here or in any later version of this skill.

## How to validate

The review file exists and names the draft it is about. Every objection quotes
the line it is about. A verdict of *send* means you found nothing in step 2,
not that you found nothing worth saying.

## What to return

`skill_return` with `status: done`, the review file in `artefacts`, and the
verdict as the first line of `summary`. `status: needs_you` when the draft
turns on a decision only the human can make, with that decision in
`open_questions`.

## What requires approval

Writing the review is an ordinary `fs_write` and is put to the user like any
other. Sending is not part of this skill and has no tool in this build; when
one exists it stays behind the human gate, and this skill still stops at the
verdict.

## What to do if the source is missing

If the draft is not at the path you were given, return `status: blocked` with
the path you tried in `open_questions`. Do not reconstruct the draft from the
conversation and review that: a review of a draft nobody will send is worse
than no review, because it reads like one.
"#;

/// The other skill a fresh library holds (PLAN 7.3, Phase 15).
pub const COS_SKILL: &str = "cos.loop";

/// `cos.loop`, the Chief-of-Staff loop as a runbook.
///
/// `COS.md` *Loop* is six steps: read the sources of truth and `/status`,
/// update the attention list, route new work, retry what is blocked, ping the
/// human only when it is irreversible, ambiguous or on a deadline, write the
/// new status and stop. Every one of them is a tool call this build already
/// has, which is exactly why it is a **skill** and not prompt text.
///
/// That is the whole argument for where this lives. PLAN 7.1 is explicit that
/// the system prompt stays a policy summary plus what is true right now, and
/// that procedure landing in it is procedure Phase 13 will have to fight. A
/// Chief of Staff whose loop was baked into the runtime would be the loop every
/// identity ran, on every turn, whether or not it was routing anything — and it
/// could not be edited by the person whose office it is. As a runbook it costs
/// one catalog line until someone runs it, it is a file in a folder they own,
/// and granting it to an identity is a separate, deliberate act (PLAN 7.6,
/// *Authoring*).
///
/// It is seeded rather than left to be written because the loop is not this
/// operator's invention — it is the mode's, it is written down in `COS.md`, and
/// a harness that shipped the handoff bus without it would be shipping the
/// verbs and none of the grammar.
const COS_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write, handoff_delegate
---

# cos.loop

## When to use it

At the start of a working session where you are routing rather than doing, and
whenever the human asks what is going on. It is the whole of your job: read the
board, decide who does what, hand it out, write down what is now true, stop.

Do not run it to do a piece of work yourself. If the answer is one file read
and one reply, that is the answer — a delegation costs another agent's turn.

## Inputs required and tools it will call

- `status/STATUS.md`, which is the board.
- `decisions/DECISIONS.md`, for what was already settled.
- `briefs/`, for work that has already gone out.
- Whatever the human just said, which is the only thing here that is new.

Calls `fs_read` to read those, `handoff_delegate` to route, and `fs_write` to
rewrite the board. It calls nothing else: a Chief of Staff that starts editing
the artefacts has stopped being one.

## Steps

1. `fs_read` `status/STATUS.md` in full. Read `decisions/DECISIONS.md` too when
   what you are about to route touches something that was decided.
2. Update the attention list in your head first: what is waiting on a person,
   what is in flight, what is blocked and on what. Anything not in one of those
   three is not on the board.
3. Route what is new. One brief per identity, each with a goal, a definition of
   done, and inputs that are *paths* — if an owner needs a document, write it
   with `fs_write` first and name the path. Hand out at most what a person could
   read in one sitting; the rest waits for the next pass.
4. Retry what is blocked, once, and only when something has changed since it
   blocked. A brief that blocks twice on the same thing is for the human, not
   for a third attempt.
5. Ping the human only when it is irreversible, ambiguous, or on a deadline.
   Those three and nothing else — a Chief of Staff who reports progress is a
   Chief of Staff who is being read past.
6. `fs_write` the board back whole, with attention, in flight and blocked each
   naming a path or an owner. Then stop. Silence is the correct output of a
   pass where nothing changed.

## How to validate

`status/STATUS.md` reads as of now: nothing is listed in flight that has come
back, nothing is under attention that nobody is waiting for. Every line names
either a path or an identity. The file is shorter than a screen; if it is not,
the detail belongs in an artefact it points at.

## What to return

`skill_return` with `status: done`, `status/STATUS.md` in `artefacts`, and a
summary of at most five lines: what changed, what is now waiting on the human,
and nothing else. `status: needs_you` when routing is blocked on a decision only
the human can make, with that decision in `open_questions`.

Never return a concatenation of what the specialists said. They each returned a
report; the board is what those add up to.

## What requires approval

`handoff_delegate` asks before anyone starts, and the write of the board is an
ordinary `fs_write` under the same gate as any other. Nothing a specialist then
does inherits your approvals: each of them is asked about its own calls, under
its own identity.

## What to do if the source is missing

If `status/STATUS.md` is not there, the workspace has not been set up for this
yet. Return `status: blocked`, say that the shared files are missing and that
the button is in the project panel. Do not create the board yourself from what
you remember — a board nobody agreed on is worse than no board.
"#;

/// `inbox.triage`, seeded into a workspace by the shared-files convention.
///
/// The stub PLAN 7.3 asks Phase 13 for: file in, status and artefact out. It
/// is deliberately not a chatbot that "knows about inboxes" — the source is a
/// markdown file in `briefs/`, which is a valid input today, and a mail
/// connector later replaces the source rather than the procedure (PLAN 7.6).
pub const TRIAGE_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# inbox.triage

## When to use it

When something has come in that has to become work: a brief in `briefs/`, a
forwarded message someone dropped in the workspace, a note from the human. One
run handles one item.

## Inputs required and tools it will call

- The item, as a path. If you were not given one, the newest unhandled file in
  `briefs/`.

Calls `fs_list` to find it, `fs_read` to read it and whatever it points at,
and `fs_write` for the two things this produces.

## Steps

1. `fs_read` the item. Decide four things and nothing else: what is being
   asked, who it is for, what it is blocked on, and whether it is urgent.
2. Write `artefacts/<name>.triage.md` — the item's path, those four answers,
   and the smallest next action that would move it.
3. `fs_read` `status/STATUS.md`, then `fs_write` it back with this item under
   **Attention** if it needs the human, **In flight** if it is now work, or
   **Blocked** with what it is waiting on. Rewrite the file whole; it is a
   board, not a log.
4. Do not answer the item, and do not act on it. Triage sorts; it does not
   deliver.

## How to validate

Both files exist, `STATUS.md` still reads on one screen, and the item appears
in exactly one of its three sections. If the item was already on the board,
the entry is updated rather than added a second time.

## What to return

`skill_return` with `status: done`, both paths in `artefacts`, and a summary of
at most five lines: what came in, where it went on the board, and the next
action. `status: needs_you` when what is being asked cannot be worked out from
the item, with the question in `open_questions`.

## What requires approval

Both writes are ordinary `fs_write` calls and are put to the user. Nothing in
this skill sends, replies or deletes.

## What to do if the source is missing

If there is no such file, or `briefs/` is empty, return `status: blocked` and
say which path you looked at. Do not triage the conversation instead: an item
nobody filed is not an item.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    use crate::policy::tool;
    use crate::store::DEFAULT_AGENT_ID;

    /// A library with `names` in it, each holding a runbook that parses.
    fn library(names: &[&str]) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        for name in names {
            write_skill(dir.path(), name, TRIAGE_SEED);
        }
        dir
    }

    fn write_skill(root: &Path, name: &str, text: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).expect("skill dir");
        fs::write(dir.join(SKILL_FILE), text).expect("runbook");
    }

    fn agent_with(skills: &[&str]) -> Agent {
        let mut agent = Agent::builtin();
        agent.id = "a1".to_owned();
        agent.name = "Triager".to_owned();
        agent.builtin = false;
        agent.skills = skills.iter().map(|name| (*name).to_owned()).collect();
        agent
    }

    #[test]
    fn a_directory_holding_a_runbook_is_a_skill() {
        let dir = library(&["inbox.triage"]);
        let found = catalog(dir.path(), None);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "inbox.triage");
        assert_eq!(found[0].scope, SkillScope::Library);
        assert_eq!(found[0].version, "1");
        assert!(found[0].runnable(), "{:?}", found[0].problem);
        assert_eq!(
            found[0].tools,
            vec![tool::FS_LIST, tool::FS_READ, tool::FS_WRITE]
        );
    }

    /// The seeded runbooks are the format's own documentation. If either stops
    /// parsing, every example a user copies is wrong.
    #[test]
    fn both_seeded_runbooks_parse() {
        for (name, text) in [(REVIEW_SKILL, REVIEW_SEED), ("inbox.triage", TRIAGE_SEED)] {
            doc::parse(text).unwrap_or_else(|err| panic!("`{name}` does not parse: {err}"));
        }
    }

    #[test]
    fn a_folder_that_is_not_a_skill_is_not_one() {
        let dir = TempDir::new().expect("temp dir");
        fs::create_dir_all(dir.path().join("notes")).expect("a plain folder");
        fs::write(dir.path().join("README.md"), "hello").expect("a plain file");
        write_skill(dir.path(), "Not A Name", TRIAGE_SEED);

        assert!(catalog(dir.path(), None).is_empty());
    }

    /// A runbook that will not parse stays in the catalog carrying its
    /// refusal. Vanishing would leave the author with nothing to fix.
    #[test]
    fn a_broken_runbook_is_listed_with_its_problem_and_never_offered() {
        let dir = TempDir::new().expect("temp dir");
        write_skill(dir.path(), "broken", "# no front matter\n");

        let found = catalog(dir.path(), None);
        assert_eq!(found.len(), 1);
        assert!(!found[0].runnable());
        assert!(found[0]
            .problem
            .as_deref()
            .is_some_and(|p| p.contains("---")));

        let agent = agent_with(&["broken"]);
        assert!(
            granted(&found, &agent).is_empty(),
            "a runbook that cannot start is not offered"
        );
    }

    #[test]
    fn a_workspace_runbook_shadows_the_library_one() {
        let lib = library(&["inbox.triage", "never-send-without-review"]);
        let work = TempDir::new().expect("temp dir");
        write_skill(&work.path().join(LIBRARY_DIR), "inbox.triage", TRIAGE_SEED);

        let found = catalog(lib.path(), Some(work.path()));

        assert_eq!(found.len(), 2, "the hidden one is not listed twice");
        let triage = find(&found, "inbox.triage").expect("found");
        assert_eq!(triage.scope, SkillScope::Workspace);
        assert!(triage.shadows, "the panel can say what is being hidden");
    }

    /// The per-agent scope. The built-in identity is the assistant from before
    /// this phase, so it holds none: a skill is always an explicit grant.
    #[test]
    fn an_identity_is_offered_only_what_it_was_granted() {
        let dir = library(&["inbox.triage", "never-send-without-review"]);
        let found = catalog(dir.path(), None);

        let triager = agent_with(&["inbox.triage"]);
        let offered: Vec<&str> = granted(&found, &triager)
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(offered, vec!["inbox.triage"]);

        let builtin = Agent::builtin();
        assert_eq!(builtin.id, DEFAULT_AGENT_ID);
        assert!(
            granted(&found, &builtin).is_empty(),
            "the built-in identity holds no skills"
        );
    }

    /// The catalog carries the line and never the steps. This is the property
    /// the phase exists for, asserted on the text that actually gets sent.
    #[test]
    fn the_catalog_block_carries_no_step_of_any_runbook() {
        let dir = library(&["inbox.triage"]);
        let found = catalog(dir.path(), None);
        let agent = agent_with(&["inbox.triage"]);

        let block = prompt_block(&granted(&found, &agent)).expect("a block");

        assert!(block.contains("inbox.triage"), "{block}");
        assert!(block.contains("v1"), "{block}");
        assert!(block.contains(tool::FS_WRITE), "{block}");
        assert!(
            !block.contains("Rewrite the file whole"),
            "a step reached the system message:\n{block}"
        );
        // The catalog is in *every* request, so what one more skill costs is
        // the number that matters — a library that grew the system message
        // without anyone noticing is exactly the drift PLAN 7.1 asks it not to
        // have. It is bounded by the summary cap plus the fixed parts of a
        // line, and nothing in a line is unbounded.
        let one = block.len();
        let two = prompt_block(&[&found[0], &found[0]])
            .expect("a block")
            .len();
        let line = two - one;
        assert!(
            line <= doc::SUMMARY_MAX_CHARS + NAME_MAX_CHARS + 64,
            "a second skill costs {line} characters"
        );
    }

    #[test]
    fn an_identity_with_no_skills_gets_no_block_at_all() {
        let dir = library(&["inbox.triage"]);
        let found = catalog(dir.path(), None);

        assert!(prompt_block(&granted(&found, &Agent::builtin())).is_none());
    }

    /// The body is read when it is invoked, not when the catalog is built.
    #[test]
    fn the_body_is_read_on_demand_and_reflects_the_file_as_it_is_now() {
        let dir = library(&["inbox.triage"]);
        let found = catalog(dir.path(), None);
        let skill = &found[0];

        assert!(load(skill)
            .expect("loads")
            .body
            .contains("Rewrite the file whole"));

        write_skill(dir.path(), "inbox.triage", REVIEW_SEED);
        assert!(
            load(skill)
                .expect("loads")
                .body
                .contains("Sending is not a step"),
            "an edited runbook is the one that runs"
        );
    }

    #[test]
    fn the_library_is_seeded_once_and_never_argues_about_it() {
        let dir = TempDir::new().expect("temp dir");
        let library = dir.path().join(LIBRARY_DIR);

        seed(&library);
        let seeded = catalog(&library, None);
        let names: Vec<&str> = seeded.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, [COS_SKILL, REVIEW_SKILL]);
        for skill in &seeded {
            assert!(skill.runnable(), "{}: {:?}", skill.name, skill.problem);
        }

        for name in [COS_SKILL, REVIEW_SKILL] {
            fs::remove_dir_all(library.join(name)).expect("the user deletes it");
        }
        seed(&library);
        assert!(
            catalog(&library, None).is_empty(),
            "a deleted example does not come back"
        );
    }

    #[test]
    fn a_run_is_tracked_from_the_envelope_and_closed_by_a_return() {
        let mut active = None;

        let opened = ToolResult::for_test(
            true,
            tool::SKILL_RUN,
            serde_json::json!({ META_SKILL: "inbox.triage" }),
        );
        track(&mut active, tool::SKILL_RUN, &opened);
        assert_eq!(active.as_deref(), Some("inbox.triage"));

        let failed = ToolResult::for_test(false, tool::SKILL_RETURN, serde_json::json!({}));
        track(&mut active, tool::SKILL_RETURN, &failed);
        assert_eq!(
            active.as_deref(),
            Some("inbox.triage"),
            "a refused return leaves the run open"
        );

        let closed = ToolResult::for_test(true, tool::SKILL_RETURN, serde_json::json!({}));
        track(&mut active, tool::SKILL_RETURN, &closed);
        assert_eq!(active, None);
    }

    #[test]
    fn a_declared_tool_the_identity_does_not_hold_is_named() {
        let held = vec![tool::FS_READ.to_owned()];
        let ctx = SkillCtx {
            library: Path::new("."),
            workspace: None,
            tools: &held,
            active: None,
        };

        let declared = vec![tool::FS_READ.to_owned(), tool::FS_WRITE.to_owned()];
        assert_eq!(ctx.missing(&declared), Some(tool::FS_WRITE));
        assert_eq!(ctx.missing(&held), None);
    }
}
