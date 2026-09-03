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
//! `.aegis/skills/`, which is where "how *this* project is deployed" belongs
//! and which travels with the folder in git. The third, per-agent, is not a
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
use crate::workspace;

pub use doc::SkillDoc;

/// The directory skills live in — in the library and in a workspace alike.
///
/// One name for both, so "where do skills go" has one answer whichever scope
/// someone is writing for. It is the *leaf*: the user's library is this
/// directory beside the other application data, and a workspace's is this
/// directory inside
/// [`workspace::CABINET_DIR`](crate::workspace::CABINET_DIR), with the rest of
/// the convention it belongs to.
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
        // Under the cabinet, not at the workspace root: a project's runbooks
        // are one directory of the same convention as its briefs and its board
        // ([`workspace`](crate::workspace)), and they moved with it. The leaf
        // name is still [`LIBRARY_DIR`], which is what keeps "where do skills
        // go" one answer in both scopes.
        let scope = root.join(workspace::CABINET_DIR).join(LIBRARY_DIR);
        for mut skill in read_dir(&scope, SkillScope::Workspace) {
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

/// Follows a session's skill run across the tool calls of one round.
///
/// The scope used to be the turn, on the argument that a local variable cannot
/// be left set by a crash, a cancel or a window closing, and that a span which
/// outlived its turn could name calls made after the conversation had moved on.
/// The first half is still true and is why this function still takes a
/// `&mut Option<String>`; the second half was measured and found to cost more
/// than it saved. A real run spans turns because the round cap ends them —
/// `IDEAS.md` § 10 has the trace, where half a run's audit lines carried no
/// skill at all, the artefact write among them, and the closing
/// `skill_return` was refused for want of anything open to close.
///
/// So the local is now seeded from the session at the top of a turn and carried
/// back at the end of it
/// ([`TurnRegistry::carry_run`](crate::agent::registry::TurnRegistry::carry_run)),
/// and the "moved on" risk is bounded there instead: a cancel closes the run,
/// and so does spending
/// [`MAX_RUN_TURNS`](crate::agent::registry::MAX_RUN_TURNS) without returning.
/// The body's scope has not changed and is not this: it is still loaded into
/// one turn and gone from the next, which is what makes a run cheap. What
/// carries is the name.
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

/// Creates the library and offers each seeded runbook to it once.
///
/// "Once" is per *name*, recorded in [`SEEDED_FILE`], rather than "once ever,
/// keyed on the directory". Both rules keep the promise that matters — a
/// runbook somebody deleted does not come back on the next start, because an
/// application arguing with its user about the contents of their own folder is
/// the thing to avoid. The manifest is what lets a later phase add a runbook
/// the mode needs without every existing install being the one install that
/// never sees it.
///
/// A library that predates the manifest is treated as having already been
/// offered [`SEEDED_BEFORE`] — the two it shipped with — so an upgrade adds the
/// new ones and resurrects nothing.
///
/// Best effort throughout. A library that could not be created costs the
/// examples and nothing else: [`catalog`] reads a missing directory as an empty
/// one.
///
/// Nine runbooks now, in three groups. Two are the halves of the mode as it
/// was: the standing rule that nothing irreversible goes out unreviewed, and
/// the loop a Chief of Staff runs. Four are the world's (PLAN 7.2) — draft one,
/// perceive a delta, verify against the oracle, check the constitution. Three
/// are the client-delivery pack (PLAN 7.3, Phase 19) — review a range, draft a
/// deploy, triage an alert.
///
/// That last group is what a **domain pack** is, and the reason it is here
/// rather than anywhere else in this tree. Phase 19's rule is *domain packs as
/// skills, not runtime*: delivery reaches the harness as three files in a
/// directory, and `agent/turn.rs`, the policy matrix and the tool registry do
/// not know that a client exists. Each runbook stops one step short of the act
/// that cannot be taken back — a merge, a deploy, a reply to somebody who is
/// waiting — because that step is the human's (PLAN 7.4) and a procedure that
/// ended with it would be a procedure that had taken it. The rest of the pack
/// is not in this file: the connectors it may want are installed by the
/// operator (Phase 18), and the specialist that runs it is an identity
/// somebody made and granted these names to.
///
/// [`DRAFT_SKILL`] is the one that writes `world/`, and it is not the
/// `world.amend` PLAN 7.2 refuses: that sentence is about *specialists*, and
/// this is refused inside a brief exactly like any other write to the
/// constitution, because the gate reads the run and not the runbook. What it
/// serves is the other half of the same paragraph — amending the world is a
/// cabinet act — where a person is in the session and would otherwise write six
/// files by hand.
///
/// None of them is granted to anything by being here. An identity that may run
/// one is an identity somebody granted it to (PLAN 7.6, *Authoring*).
pub fn seed(library: &Path) {
    let manifest = library.join(SEEDED_FILE);
    let mut offered: Vec<String> = match fs::read_to_string(&manifest) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        // A library from before the manifest existed. It has already been
        // offered what it shipped with, whether or not those are still in it.
        Err(_) if library.is_dir() => SEEDED_BEFORE.iter().map(|&name| name.to_owned()).collect(),
        Err(_) => Vec::new(),
    };

    let mut written = 0usize;
    for (name, body) in SEEDED {
        if offered.iter().any(|seen| seen == name) {
            continue;
        }

        let dir = library.join(name);
        if let Err(err) = fs::create_dir_all(&dir) {
            tracing::warn!(%err, dir = %dir.display(), "could not create the skill library");
            return;
        }
        if let Err(err) = fs::write(dir.join(SKILL_FILE), body) {
            tracing::warn!(%err, name, "could not write an example skill");
            return;
        }
        offered.push((*name).to_owned());
        written += 1;
    }

    if written == 0 {
        return;
    }
    // Written after the runbooks, not before: a crash between the two leaves a
    // name unrecorded, which costs one redundant write on the next start. The
    // other order would lose the runbook for good.
    if let Err(err) = fs::write(&manifest, format!("{}\n", offered.join("\n"))) {
        tracing::warn!(%err, path = %manifest.display(), "could not record what was seeded");
    }
    tracing::info!(dir = %library.display(), written, "example runbooks seeded");
}

/// Where the library records which runbooks it has already been offered.
///
/// A dotfile inside the library rather than a key in a store: the library is a
/// directory of directories and this is a fact about that directory. [`read_dir`]
/// skips it for free — it is not a folder holding a `SKILL.md`.
const SEEDED_FILE: &str = ".seeded";

/// What a library created before [`SEEDED_FILE`] existed has already been
/// offered.
const SEEDED_BEFORE: [&str; 2] = [REVIEW_SKILL, COS_SKILL];

/// Every runbook this build seeds, and the body each starts as.
const SEEDED: [(&str, &str); 9] = [
    (REVIEW_SKILL, REVIEW_SEED),
    (COS_SKILL, COS_SEED),
    (DRAFT_SKILL, DRAFT_SEED),
    (PERCEIVE_SKILL, PERCEIVE_SEED),
    (VERIFY_SKILL, VERIFY_SEED),
    (CHECK_SKILL, CHECK_SEED),
    (REVIEW_DIFF_SKILL, REVIEW_DIFF_SEED),
    (DEPLOY_SKILL, DEPLOY_SEED),
    (ALERT_SKILL, ALERT_SEED),
];

/// The standing rule of the whole mode, as a runbook.
pub const REVIEW_SKILL: &str = "never-send-without-review";

/// `never-send-without-review`, the first skill a fresh library holds.
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
3. Write the review to `.aegis/artefacts/<draft name>.review.md`. Say what you
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

/// The Chief of Staff's own loop (PLAN 7.3, Phase 15).
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

- `.aegis/status/STATUS.md`, which is the board.
- `.aegis/decisions/DECISIONS.md`, for what was already settled.
- `.aegis/briefs/`, for work that has already gone out.
- The world's status, if this workspace has one: your system message says
  whether `world/` is in force and whether any of its declared sources have
  moved.
- Whatever the human just said, which is the only thing here that is new.

Calls `fs_read` to read those, `handoff_delegate` to route, and `fs_write` to
rewrite the board. It calls nothing else: a Chief of Staff that starts editing
the artefacts has stopped being one.

## Steps

0. If this workspace has a world and its declared sources have drifted, that is
   the attention item, and it comes before routing. One bounded
   `world.perceive-delta` over the paths that moved, and nothing else this pass:
   a brief that is not about the delta will not launch anyway, and a compile on
   top of a source nobody has re-read is an instance built from a schema that is
   already wrong. If the delta comes back saying the essence would have to move,
   that is for the human — amending the world is not yours.
1. `fs_read` `.aegis/status/STATUS.md` in full. Read `.aegis/decisions/DECISIONS.md` too when
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

`.aegis/status/STATUS.md` reads as of now: nothing is listed in flight that has come
back, nothing is under attention that nobody is waiting for. Every line names
either a path or an identity. The file is shorter than a screen; if it is not,
the detail belongs in an artefact it points at.

## What to return

`skill_return` with `status: done`, `.aegis/status/STATUS.md` in `artefacts`, and a
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

If `.aegis/status/STATUS.md` is not there, the workspace has not been set up for this
yet. Return `status: blocked`, say that the shared files are missing and that
the button is in the project panel. Do not create the board yourself from what
you remember — a board nobody agreed on is worse than no board.
"#;

/// Help with founding or amending a world (PLAN 7.2).
pub const DRAFT_SKILL: &str = "world.draft";

/// `world.draft`, the one runbook in this set that writes `world/`.
///
/// PLAN 7.2 says there is no specialist skill `world.amend`, and there is not:
/// this is refused outright inside a brief, like every other write to the
/// constitution, because the gate reads the *run* and not the runbook. What it
/// is for is the other side of that sentence — *amending the world is a cabinet
/// act* — where a person is in the session asking for help writing six files
/// they would otherwise write alone.
///
/// The distinction it has to hold, and the reason most of its steps are about
/// stopping: drafting is a procedure, and deciding is not. An agent is good at
/// reading a repository and proposing what its schema *is*; it is not the thing
/// that decides what a project is *for*, or what would make a new instance
/// right. So the runbook writes the descriptive files from evidence and refuses
/// to invent the essence or the oracle, which are the two the human owns.
const DRAFT_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# world.draft

## When to use it

When a person asks for help founding this workspace's world, or amending one
after a delta was perceived. Only in a session somebody is sitting in: inside a
brief every write to `world/` is refused, and that refusal is the point — a
specialist does not change what the project is.

Not for a workspace that has nothing to protect. A watch folder or a wish list
has no essence and no oracle, and six templates in one are worse than nothing.
If you cannot say in one line what this project *is*, say so and stop.

## Inputs required and tools it will call

- Whatever the project already says about itself: a README, the module headers,
  an existing `world/`, `.aegis/decisions/DECISIONS.md`.
- What the human tells you it is for. That part is not in the files.

Calls `fs_list` and `fs_read` to gather, and `fs_write` for each file it
drafts. Every write is put to the person, and they may allow the rest of the
session in one answer — that answer is theirs to give, not something to ask for
twice.

## Steps

1. `fs_read` `world/` first if it is there. You are amending, not starting
   over: what is already written stands unless the human says it moves.
2. Gather. Read the README, the entry points, the module headers, the decisions
   already filed. Do not open anything `world/sources.yml` declares — those are
   perceived already, and a read of one that has not changed is refused.
3. Ask the human two questions and wait for the answers: what is this for, and
   how would you know a new version of it was right. Do not guess either. They
   are `essence.md` and `oracle.md`, and they are the two files that make the
   others worth having.
4. Draft what the evidence supports, one `fs_write` at a time, smallest first:
   `world/schema.md` (the shapes you actually found, named as they are named in
   the code) and `world/behaviours.md` (how it behaves — and for anything that
   reads like a rule, the perimeter it holds inside: this contract, this role,
   this class of tasks. A local failure is not an invariant).
5. Write `world/essence.md` and `world/oracle.md` from the human's answers, in
   their words. Quote them rather than improving them.
6. Say what you left empty and why. A world with four files is a world; a world
   with six files two of which you invented is a liability.

## How to validate

Every line in `schema.md` and `behaviours.md` can be traced to a file you read,
and you can name which. Nothing in `essence.md` or `oracle.md` is yours. No
file restates another. A person who has never seen this project can read
`essence.md` and say what it is for.

## What to return

`skill_return` with `status: done`, every file you wrote in `artefacts`, and a
summary of at most five lines: what the world now says, and what is still
blank. `status: needs_you` when the human has not answered step 3 — that is not
a blocker to work around, it is the work.

## What requires approval

Every write into `world/` is put to the person, at high risk, and they may
allow the rest of the session in one answer. Nothing else here is granted by
it: a standing approval for the constitution covers the constitution.

If a write is refused outright rather than asked about, you are inside a brief.
Stop and return `needs_you`: founding or amending a world is not delegated
work.

## What to do if the source is missing

If there is nothing to read — an empty folder, no README, no code — say so and
return `status: blocked`. A world drafted from nothing is six files of
plausible prose, which is the most expensive possible thing to have to unlearn.
"#;

/// The delta half of the world's library (PLAN 7.2, *Library skills*).
pub const PERCEIVE_SKILL: &str = "world.perceive-delta";

/// `world.perceive-delta`, the only legitimate re-perception there is.
///
/// A world is perceived once. What may then change is a declared source — the
/// operator drops a new dump, a log grew — and that delta is the one thing
/// worth reading a source artefact for. The runbook is deliberately narrow: it
/// takes the paths whose hash moved, and it returns a *proposal*. It does not
/// write `world/`, because nothing a specialist runs does.
const PERCEIVE_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write
---

# world.perceive-delta

## When to use it

Only when a declared source of this world has moved: a new dump, a log that
grew, an export regenerated. The frame in your system message names the paths,
and so does the project panel. One run covers the paths you were given and
nothing else.

Do not run it to "understand the project". The project is in `world/`, it was
perceived once, and reading the whole dump again is the round-trip this world
exists to have paid once.

## Inputs required and tools it will call

- The paths whose hash moved, as paths. If you were not given any, this runbook
  does not apply.
- `world/essence.md` and `world/schema.md`, so the delta is read against what is
  already known rather than from nothing.

Calls `fs_read` for those, and `fs_write` for the one file it produces.

## Steps

1. `fs_read` the world first — essence, then schema, then behaviours if there is
   one. What is in there is given.
2. `fs_read` the moved sources. Read the *delta*: what is in them that the world
   does not already account for, and what in the world they now contradict. Skip
   whatever only confirms what you have already read.
3. Write `.aegis/artefacts/world-delta-<date>.md`: one section per moved path, and in
   each, what is new, what is contradicted, and — for anything that looks like
   an invariant — the perimeter it holds inside (this contract, this role, this
   class of tasks). A local failure is not an invariant; say so when that is
   what you found.
4. End the file with the amendment you are proposing, written as the lines that
   would change in `world/`, and the `bytes` and `sha256` each moved source
   should now be recorded at in `world/sources.yml`.
5. Stop. Do not write `world/`.

## How to validate

Every claim in the file names the path and the place in it that supports it.
Nothing in it restates what `world/essence.md` already says. Every proposed
invariant carries a perimeter.

## What to return

`skill_return` with `status: done`, the delta file in `artefacts`, and a summary
of at most five lines: which sources moved, what changed in the world's own
terms, and whether the essence would have to move. `status: needs_you` when it
would — that is an écart, and it belongs to the human.

## What requires approval

The write of the delta file is an ordinary `fs_write` under the usual gate. A
write into `world/` is refused outright, whatever this file proposes: amending
the constitution is a human decision, taken by whoever owns the world.

## What to do if the source is missing

If a path you were given is not there, say so and stop: a declared source that
has vanished is an attention item for the human, not something to reconstruct.
Return `status: blocked` with the path in `open_questions`.
"#;

/// The verifier half of the world's library (PLAN 7.2, *Library skills*).
pub const VERIFY_SKILL: &str = "world.verify";

/// `world.verify`, the oracle as a program rather than a taste review.
///
/// `COS.md` *Work*: verification is a program, not a reviewer's opinion about a
/// diff, and evidence is paths. The instance is disposable; what is not is
/// whether it satisfies the oracle. This is the runbook a fan-in reviewer runs,
/// and the reason the Chief of Staff does not re-read the work itself.
const VERIFY_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write, shell_exec
---

# world.verify

## When to use it

When an instance has been produced and somebody has to know whether it is right:
after a compile brief comes back, before anything built on it is used, and
whenever the oracle itself has changed.

Not a code review, and not a judgement about the shape of a diff. The question
is one question — does this satisfy `world/oracle.md` — and it has an answer.

## Inputs required and tools it will call

- `world/oracle.md`, which is the criterion.
- The instance, as paths.
- Whatever the oracle names as the way it is checked: a command, a scenario
  file, a characterisation.

Calls `fs_read` for the oracle and the instance, `shell_exec` to run whatever
the oracle says the check is, and `fs_write` for the verdict.

## Steps

1. `fs_read` `world/oracle.md`. Turn it into a list of clauses, each of which is
   either satisfied or not. A clause you cannot decide is a defect in the
   oracle — record it as one rather than deciding it by feel.
2. Run the checks. Where the oracle names a command, `shell_exec` it and keep
   the output. Where it names a scenario, run the scenario. Do not substitute
   reading the code for running the check.
3. Write `.aegis/artefacts/<instance>.verified.md`: one line per clause with *pass*,
   *fail* or *undecidable*, each naming the path or the command output that says
   so. A pass with no evidence is a fail.
4. If a clause fails and nothing in `world/` changed, say so plainly: the
   instance is wrong, not the world. Regenerating it is legal and cheap; arguing
   with the oracle is not yours to do.

## How to validate

Every clause of the oracle appears exactly once in the verdict. Every verdict
line names its evidence as a path or as command output. The file says *pass*
only if every clause did.

## What to return

`skill_return` with `status: done`, the verdict file in `artefacts`, and the
overall pass or fail as the first line of `summary`. `status: needs_you` when a
clause is undecidable as written — that is the oracle needing an amendment,
which is the human's.

## What requires approval

Every command is an ordinary `shell_exec` and is put to the user with its
arguments and working directory. Nothing here writes `world/`: the oracle is
read, never edited.

## What to do if the source is missing

If there is no `world/oracle.md`, return `status: blocked` and say that this
world has no oracle yet. Do not invent one from the instance — an oracle derived
from the thing it is meant to judge says nothing.
"#;

/// The world's own read, for a session the frame did not open
/// (PLAN 7.2, *Library skills*).
pub const CHECK_SKILL: &str = "world.check";

/// `world.check`, the frame as a procedure.
///
/// The frame is injected into every session whose workspace has a world, so a
/// session working on one already knows the rule. What it does not have is a
/// step that reads the constitution deliberately and says where it stands. That
/// is this: cheap, read-only, and the honest first move in a session about to
/// touch a world it has not read.
const CHECK_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read
---

# world.check

## When to use it

At the start of any session about to work on a world that did not open as a
Chief of Staff pass: before writing an instance, before answering a question
about what the project *is*, and whenever you are unsure whether what you are
about to do is an écart.

## Inputs required and tools it will call

- Nothing. It reads `world/`.

Calls `fs_list` and `fs_read`, and nothing else. It writes nothing, here or in
any later version of it.

## Steps

1. `fs_list` `world/`. Note which of `essence.md`, `schema.md`, `behaviours.md`,
   `oracle.md` and `decisions.md` are there. A missing one is a fact about this
   world, not an error.
2. `fs_read` the essence, then the schema, then the behaviours. Read the sins
   with their perimeters: a behaviour that binds one contract is not a rule
   about everything.
3. `fs_read` `world/oracle.md` if there is one. That is what any instance will
   be judged against, and it is worth knowing before writing one rather than
   after.
4. Check what you were asked to do against what you have just read. If doing it
   would change the essence, stop: that is an écart, and it is the answer.
5. Do not read what `world/sources.yml` declares. Those have been perceived, and
   a read of one that has not changed is refused.

## How to validate

You can say, in three lines and without opening the files again, what this thing
is, what it is checked against, and which recorded behaviour is closest to what
you were asked to do.

## What to return

`skill_return` with `status: done` and a summary of at most five lines: what the
world says this is, what the oracle checks, and whether the work in front of you
is inside the essence or an écart. `status: needs_you` when it is an écart, with
the line of the essence that would have to move in `open_questions`.

## What requires approval

Nothing. Every read is inside the workspace and happens without asking. There is
no write step, and this is not the place to add one: amending the world is a
human decision.

## What to do if the source is missing

If there is no `world/`, this workspace has no constitution and this runbook
does not apply. Return `status: blocked`, say so in one line, and get on with
the work under the cabinet's own rules.
"#;

// ---------------------------------------------------------------------------
// The client-delivery pack (PLAN 7.3, Phase 19, pack 1)
// ---------------------------------------------------------------------------

/// Review a range before it goes to a client (PLAN 7.6 names it, under *Three
/// scopes*).
pub const REVIEW_DIFF_SKILL: &str = "review.diff";

/// `review.diff`, the first runbook of the delivery pack.
///
/// The pack's cheapest runbook, and the one that needs nothing installed: `git`
/// is on the machine of anybody who has a client repository, so the source of
/// a diff is `shell_exec` today and a forge's connector later — the procedure
/// does not move when it does (PLAN 7.6).
///
/// What makes it a runbook rather than a prompt is step 4. "Review this diff"
/// gets a model's four best observations about the hunks it was shown; a fixed
/// order that asks about intent, correctness, irreversibility and leakage gets
/// the same four questions on a Friday as on a Tuesday, and a verdict that can
/// be compared with last week's. The step that reads the *files* rather than
/// the hunks is there for the failure this catches most often: a hunk is
/// correct and what it now sits beside is not.
const REVIEW_DIFF_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write, shell_exec
---

# review.diff

## When to use it

Before a change goes to a client: a branch about to become a pull request, one
waiting on you, or a patch file in the workspace.

One run reviews one range. Prefer not to run it on a change you wrote in this
session — a second opinion from whoever had the first one is worth less than it
looks; hand it to another identity, or to the human.

## Inputs required and tools it will call

- The range, as two revisions: the base the change will land on and its tip.
  `main..HEAD` is the usual one. Or, if you were handed a `.diff` or `.patch`
  file instead, its path.
- What the change was meant to do, in one line. Without it a review is a list
  of things somebody noticed, not a judgement.

Calls `shell_exec` for read-only git (`diff`, `log`, `show`, `status`),
`fs_read` for the files as they now stand, and `fs_write` for the review.

## Steps

1. Establish the range. `git status`, then `git log --oneline <base>..<tip>`, so
   the review names commits that exist rather than a branch that has moved
   since somebody described it to you.
2. `git diff --stat <base>..<tip>` first, then the diff itself. If the change
   touches more than you were told it would, that is already the first finding.
3. `fs_read` every file the diff changes, at its current state. A hunk is not
   the file: the defect is usually in what the hunk now sits next to.
4. Judge four things, in this order and no other: does it do what it was meant
   to do; is anything in it wrong; is anything in it irreversible once merged —
   a migration, a dropped column, a rotated key; and does it put something in
   the client's repository that should not be there.
5. `fs_write` the review to `.aegis/artefacts/<branch>.review.md`: the range,
   the four answers, then one finding per bullet, each naming `path:line` and
   quoting the line it is about. End with one of *ship*, *change first* or
   *do not ship*.
6. Stop. Merging, pushing, tagging and answering the pull request are not steps
   here, and a later version of this runbook that added them would not make
   them yours.

## How to validate

The review names the exact revisions it read, and `git log` still shows them.
Every finding gives a path and quotes its line. A verdict of *ship* means you
worked through step 4 and found nothing, not that nothing stood out.

## What to return

`skill_return` with `status: done`, the review file in `artefacts`, the range
in `evidence`, and the verdict as the first line of `summary`.
`status: needs_you` when the change turns on a decision that is the client's or
the human's — a behaviour nobody asked for, a dependency with a licence — with
that decision in `open_questions`.

## What requires approval

Every `git` command is an ordinary `shell_exec`, put to the user with its
arguments. Keep them read-only: `diff`, `log`, `show`, `status`. A command that
moves the repository — `checkout`, `merge`, `push`, `reset`, `stash` — is not
part of a review, and the working tree you are reading belongs to somebody who
did not ask you to touch it.

Read-only means the working tree too, and this is where it is easy to be wrong:
a build or a test command **writes**. A filtered `cargo test` in this repository
regenerates a tracked file from a subset of its types and truncates the rest. If
you need the suite to judge the change, say so and let the person run it; if you
run it anyway, you have changed the tree you are reviewing and the review has to
say so.

## What to do if the source is missing

If the range does not resolve, or the patch is not at the path you were given,
return `status: blocked` with what you tried in `open_questions`. Do not review
the conversation's account of the change: a review of a diff nobody read is
worse than no review, because it reads like one.

**A refusal is a missing source.** If a read you needed was denied — by the
person, or by the round limit that ends a turn — the review is partial, and that
is a fact about the review rather than an accident of how it went. Name the file
you could not read, and return `status: needs_you`. Never write *ship* on a diff
you were refused: a verdict on a file nobody opened is the exact failure the
four questions exist to prevent.

If `.aegis/artefacts/` is not there, this workspace has not been set up for the
cabinet. Write the review with its directory created, and say in `summary` that
the shared files are missing — the button is in the project panel.
"#;

/// Draft a deployment somebody else runs (PLAN 7.3, Phase 19: *destructive
/// deploy stays gated*).
pub const DEPLOY_SKILL: &str = "deploy.draft";

/// `deploy.draft`, the runbook that stops one step before the deploy.
///
/// The pack's whole argument in one file. A deploy is irreversible in the sense
/// the matrix means — somebody else's users are on the other end — so the
/// procedure that can be written down is everything up to it, and the act
/// itself stays a human one (PLAN 7.4). That is not a limitation this runbook
/// works around later: there is no version of it that ends with the deploy,
/// which is why the last step says so rather than leaving it to the gate.
///
/// It also fixes where the facts come from. The project's own account — a
/// workspace runbook, the compose file, the CI workflow — beats what a model
/// knows about how applications like this are usually shipped, and a missing
/// account is a `blocked` rather than an invitation to infer a pipeline. A
/// Coolify or a host connector, when there is one, replaces that source and
/// leaves the procedure alone (PLAN 7.6).
const DEPLOY_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write, shell_exec
---

# deploy.draft

## When to use it

When something is ready to go to an environment and a person has to decide. The
output is a plan somebody reads and runs; this runbook never deploys.

## Inputs required and tools it will call

- Which revision, and which environment. Both by name — "the latest" is not a
  revision, and "prod" is not an environment unless that is the project's own
  word for it.
- Where the project says how it is deployed: `.aegis/skills/` if this workspace
  has its own deploy runbook, then the README, the compose file, the
  Dockerfile, the CI workflow. Those are the source. What you know about how
  applications like this are usually shipped is not.

Calls `fs_list` and `fs_read` to gather, `shell_exec` for read-only checks, and
`fs_write` for the plan.

## Steps

1. Read the project's own account first. A workspace runbook that says how
   *this* application ships beats everything else here, including this file.
2. Pin the revision. `git log -1 <revision>` so the plan names a commit that
   exists, and say what is in it that is not in what is running.
3. List what the deploy changes beyond code: migrations, environment variables,
   a queue that must drain, a cache to clear, a job to stop first. Name the
   variables. Never read a value into the plan.
4. `fs_write` `.aegis/artefacts/deploy-<environment>-<short revision>.md`: the
   revision, the environment, and the commands in the order a person runs them,
   one per line. Beside each one that changes data, how it is undone. A step
   with no way back is marked as one, in words, on its own line.
5. Say what "it worked" looks like: the check to run afterwards and what it
   should say. A plan with no answer to that is a plan nobody can stop halfway
   through.
6. Stop. Do not run the plan — not even its first read-only step, to be sure.
   The person who decides to deploy is the person who runs it.

## How to validate

The plan names one revision and one environment. Every command is copy-pastable
as written, with no placeholder the reader has to guess at. Every step that
changes data carries a rollback line or is marked irreversible. No secret value
appears anywhere in the file — only names.

## What to return

`skill_return` with `status: done`, the plan in `artefacts`, the revision in
`evidence`, and a summary of at most five lines: what ships, where, and which
steps cannot be undone. `status: needs_you` when the plan cannot be written
without a decision — a migration that drops data, a window that costs users —
with that decision in `open_questions`.

## What requires approval

Reads inside the workspace happen without asking. Each `shell_exec` is put to
the user with its arguments; keep them read-only, and count a build or a test
command as a write — it touches the tree you are describing. Deploying is not a
step in this runbook and there is no version of it in which it is. When a host's
connector exists it will replace where these facts come from, not who presses
go.

## What to do if the source is missing

If the project does not say how it is deployed, return `status: blocked`, name
the files you looked in, and ask for the one that is missing. Do not draft from
the framework's defaults: a plausible deploy for an application that is shipped
some other way is the most expensive artefact in this pack.

A refusal is a missing source: a read denied by the person, or by the round
limit that ends a turn, leaves a plan resting on a file you never opened. Say
which one and return `status: needs_you`. If `.aegis/artefacts/` is not there,
write the plan with its directory created and say the shared files are missing.
"#;

/// Turn a monitoring signal into a note and a reply nobody has sent.
pub const ALERT_SKILL: &str = "alert.draft";

/// `alert.draft`, triage for something that is on fire.
///
/// Two files out, and the second one is why this is in the pack: an incident is
/// the moment a client is owed a sentence, and that sentence is the one most
/// likely to be written out of an inference somebody stopped marking as one. So
/// the note keeps observed and inferred apart and cites the command behind
/// every observed line, and the reply may not carry a cause the note marked as
/// a guess.
///
/// The steps that say *stop* are load-bearing here in a way they are not in
/// [`REVIEW_DIFF_SEED`]. Restarting the service is the one move in an incident
/// that is both plausible and destroys the evidence for what caused it; it is
/// also exactly what a model holding `shell_exec` and a sense of helpfulness
/// reaches for. Triage produces the note that informs that decision. The
/// decision stays a person's.
const ALERT_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write, shell_exec
---

# alert.draft

## When to use it

When monitoring has fired, or a client says something is broken. It turns the
signal into an incident note and a reply the human may send.

It does not fix anything, and it does not answer anybody.

## Inputs required and tools it will call

- The alert, as a path: the exported alert, the log excerpt, the message
  somebody dropped in `.aegis/briefs/`. If you were given no path, the newest
  unhandled file there.
- Which system it is about, if the alert does not say.

Calls `fs_list` and `fs_read` for the alert and what it points at, `shell_exec`
for read-only checks, and `fs_write` for the two drafts.

## Steps

1. `fs_read` the alert whole, including the parts that repeat. When it started,
   how often it has fired, and what it actually measures are the three facts a
   reply stands on.
2. Establish what is true now, with read-only commands: the service's own health
   output, the last lines of a log, `git log -1` on what is deployed. **Four
   commands, and the fourth is the last.** A count rather than "enough", because
   an alert that names something broken is an invitation to go and fix it, and
   refusing that invitation is most of this runbook's job. If four have not
   established the cause, *that is the finding*: write the note with the cause
   marked unestablished and propose the diagnosis as the next action instead of
   starting it. A run that spends twenty commands has stopped triaging and is
   debugging under another name, unwatched, with nobody expecting it.
3. Keep what you observed and what you infer apart, and keep them apart for the
   rest of the run. Every line of the note is one or the other and says which.
4. `fs_write` `.aegis/artefacts/incident-<date>-<system>.md`: when it started,
   what is affected, what is *not* affected, what is true right now, the
   likeliest cause with how sure you are, and the next action you would take.
   Cite the command or the file behind every observed line.
5. `fs_write` the reply beside it, named for the same incident with `.reply`
   before the extension: what happened, what it means for them, what is being
   done, and when they will hear next. It promises nothing the note does not
   support, and it names no cause the note marked as inferred.
6. Stop. Restarting a service, scaling something, clearing a queue or rolling
   back are not steps here — they are the decision this note exists to inform.
   Sending the reply is the human's, after `never-send-without-review`.

## How to validate

Every observed line in the note cites a command or a file, with the time it came
from, and the note cites **at most four commands** — if it cites more, this was
not a triage. The reply contains no claim the note does not carry. The note says
what is *unaffected*: a report that lists only damage cannot be used to decide
anything.

## What to return

`skill_return` with `status: done`, both files in `artefacts`, the checks you
ran in `evidence`, and a summary of at most five lines: what is affected, what
is not, and the next action. `status: needs_you` when that action is
irreversible or reaches users — which is most of the interesting ones — with it
in `open_questions`.

## What requires approval

Both writes are ordinary `fs_write` calls. Every check is a `shell_exec` put to
the user with its arguments; keep them read-only — a build or a test command
writes, and during an incident it competes with the thing you are diagnosing —
and remember that the "harmless" restart is the one command that destroys the
evidence for what caused it. Nothing here sends: the reply is a file until a
person sends it.

## What to do if the source is missing

If there is no alert at the path you were given and nothing unhandled in
`.aegis/briefs/`, return `status: blocked` and say which path you looked at. Do
not write an incident note from a dashboard you cannot see, and do not
reconstruct the alert from what the conversation said it probably was.

A check you were refused — by the person, or by the round limit that ends a turn
— is not an observation and does not become one. Leave it out of the note, say
in `summary` what you could not check, and return `status: needs_you` if the
next action turns on it. If `.aegis/artefacts/` is not there, write both files
with their directory created and say the shared files are missing.
"#;

/// `inbox.triage`, seeded into a workspace by the shared-files convention.
///
/// The stub PLAN 7.3 asks Phase 13 for: file in, status and artefact out. It
/// is deliberately not a chatbot that "knows about inboxes" — the source is a
/// markdown file in `.aegis/briefs/`, which is a valid input today, and a mail
/// connector later replaces the source rather than the procedure (PLAN 7.6).
pub const TRIAGE_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# inbox.triage

## When to use it

When something has come in that has to become work: a brief in `.aegis/briefs/`, a
forwarded message someone dropped in the workspace, a note from the human. One
run handles one item.

## Inputs required and tools it will call

- The item, as a path. If you were not given one, the newest unhandled file in
  `.aegis/briefs/`.

Calls `fs_list` to find it, `fs_read` to read it and whatever it points at,
and `fs_write` for the two things this produces.

## Steps

1. `fs_read` the item. Decide four things and nothing else: what is being
   asked, who it is for, what it is blocked on, and whether it is urgent.
2. Write `.aegis/artefacts/<name>.triage.md` — the item's path, those four answers,
   and the smallest next action that would move it.
3. `fs_read` `.aegis/status/STATUS.md`, then `fs_write` it back with this item under
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

If there is no such file, or `.aegis/briefs/` is empty, return `status: blocked` and
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

    /// The seeded runbooks are the format's own documentation. If one stops
    /// parsing, every example a user copies from it is wrong.
    #[test]
    fn every_seeded_runbook_parses() {
        for (name, text) in SEEDED.iter().chain([&("inbox.triage", TRIAGE_SEED)]) {
            doc::parse(text).unwrap_or_else(|err| panic!("`{name}` does not parse: {err}"));
        }
    }

    /// The delivery pack (PLAN 7.3, Phase 19) calls nothing that has to be
    /// installed first.
    ///
    /// A pack is skills, connectors and an identity, and only the first of
    /// those ships. A seeded runbook that declared `coolify__deploy` would be
    /// an example that fails closed at `skill_run` on every machine where that
    /// connector is not installed — which is every machine, on the day it is
    /// seeded. The source of a diff or a deploy fact is `shell_exec` and the
    /// project's own files today; a connector replaces the source later, and
    /// that is an edit to the runbook the operator owns.
    #[test]
    fn the_delivery_pack_calls_only_tools_this_build_has() {
        for name in [REVIEW_DIFF_SKILL, DEPLOY_SKILL, ALERT_SKILL] {
            let (_, text) = SEEDED
                .iter()
                .find(|(seeded, _)| *seeded == name)
                .unwrap_or_else(|| panic!("`{name}` is seeded"));
            let doc = doc::parse(text).unwrap_or_else(|err| panic!("`{name}`: {err}"));

            assert!(!doc.tools.is_empty(), "`{name}` declares what it calls");
            for tool in &doc.tools {
                assert!(
                    crate::tools::spec(tool).is_some(),
                    "`{name}` declares `{tool}`, which needs something installed"
                );
            }
        }
    }

    /// A pack is assembled by a grant, not by being shipped.
    ///
    /// The three runbooks are in everybody's library from the first start and
    /// reach no model until an identity holds them — which is the whole of what
    /// "domain packs as skills, not runtime" costs an install that does no
    /// client work: three folders it can delete.
    #[test]
    fn the_delivery_pack_reaches_a_specialist_and_nobody_else() {
        let dir = TempDir::new().expect("temp dir");
        seed(dir.path());
        let catalog = catalog(dir.path(), None);

        let specialist = agent_with(&[REVIEW_DIFF_SKILL, DEPLOY_SKILL, ALERT_SKILL]);
        let held: Vec<&str> = granted(&catalog, &specialist)
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(held, [ALERT_SKILL, DEPLOY_SKILL, REVIEW_DIFF_SKILL]);

        assert!(
            granted(&catalog, &Agent::builtin()).is_empty(),
            "installing a pack grants nothing"
        );
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
        write_skill(
            &work.path().join(workspace::CABINET_DIR).join(LIBRARY_DIR),
            "inbox.triage",
            TRIAGE_SEED,
        );

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
        let mut names: Vec<&str> = seeded.iter().map(|skill| skill.name.as_str()).collect();
        names.sort_unstable();
        let mut expected: Vec<&str> = SEEDED.iter().map(|(name, _)| *name).collect();
        expected.sort_unstable();
        assert_eq!(names, expected);
        for skill in &seeded {
            assert!(skill.runnable(), "{}: {:?}", skill.name, skill.problem);
        }

        for (name, _) in SEEDED {
            fs::remove_dir_all(library.join(name)).expect("the user deletes it");
        }
        seed(&library);
        assert!(
            catalog(&library, None).is_empty(),
            "a deleted example does not come back"
        );
    }

    /// The manifest is what lets a later phase add a runbook the mode needs
    /// without every existing install being the one install that never sees it
    /// — and it must still not resurrect what somebody deleted.
    #[test]
    fn a_library_from_before_the_manifest_gains_the_new_runbooks_and_nothing_else() {
        let dir = TempDir::new().expect("temp dir");
        let library = dir.path().join(LIBRARY_DIR);

        // A library as an older build left it: the two it shipped with, no
        // manifest, and one of them since deleted by its owner.
        write_skill(&library, COS_SKILL, COS_SEED);

        seed(&library);
        let mut names: Vec<String> = catalog(&library, None)
            .into_iter()
            .map(|skill| skill.name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                ALERT_SKILL,
                COS_SKILL,
                DEPLOY_SKILL,
                REVIEW_DIFF_SKILL,
                CHECK_SKILL,
                DRAFT_SKILL,
                PERCEIVE_SKILL,
                VERIFY_SKILL
            ],
            "the world's runbooks and the delivery pack arrive; the deleted review does not come \
             back"
        );

        // And the manifest now covers all five, so a third start writes nothing.
        fs::remove_dir_all(library.join(CHECK_SKILL)).expect("the user deletes it");
        seed(&library);
        assert!(
            !library.join(CHECK_SKILL).exists(),
            "once offered, a runbook is the user's to keep or delete"
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
