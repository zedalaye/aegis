//! The skill runner (Phase 13; PLAN 7.6; `COS.md` *Skills*).
//!
//! A skill is a versioned `SKILL.md` runbook that sequences tools toward a done
//! criterion.
//!
//! * **Catalog in, body on demand.** The system message carries one line per
//!   granted skill ([`prompt_block`]); the body is read only by `skill_run`,
//!   into that turn. [`Skill`] has no body field, so this holds by type.
//! * **Scopes.** The user's library and the workspace's `.aegis/skills/` are
//!   directories (a workspace skill shadows a library one of the same name);
//!   the per-agent scope is [`Agent::skills`](crate::store::Agent::skills),
//!   applied by [`granted`].
//! * **No extra rights.** The skill allow-list is checked in
//!   [`policy::decide_call`](crate::policy::decide_call) with no approval
//!   offered, and a run whose declared `tools` the identity does not hold fails
//!   closed at `skill_run`. Every step is an ordinary gated call whose audit
//!   line names the skill.

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

/// The skills directory's name, in the library and under
/// [`workspace::CABINET_DIR`](crate::workspace::CABINET_DIR) alike.
pub const LIBRARY_DIR: &str = "skills";

/// The runbook inside a skill's directory, which is named after the skill and
/// may hold attachments.
pub const SKILL_FILE: &str = "SKILL.md";

/// A proposed runbook not yet applied (PLAN 7.13), beside where its `SKILL.md`
/// would go. [`read_dir`] never reads it, so a proposal cannot run.
pub const PROPOSAL_FILE: &str = "PROPOSAL.md";

/// Longest skill name.
pub const NAME_MAX_CHARS: usize = 64;

/// Most skills one catalog holds, since the catalog is in every request.
pub const CATALOG_MAX: usize = 64;

// ---------------------------------------------------------------------------
// IPC payloads
// ---------------------------------------------------------------------------

/// Which directory a skill was found in. The per-agent scope is an allow-list,
/// not a place.
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

/// One catalog entry: everything except the body, which only a run
/// [`load`]s.
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
    /// Whether this workspace skill hides a library one of the same name, so
    /// the panel can say so.
    pub shadows: bool,
    /// Why this one cannot run. Kept in the catalog for its author, never
    /// offered to the model ([`granted`]).
    pub problem: Option<String>,
}

impl Skill {
    /// Whether this entry can actually be run.
    pub const fn runnable(&self) -> bool {
        self.problem.is_none()
    }
}

/// Whether `name` is a skill name — shared by discovery and the identity
/// allow-list ([`store::agents`](crate::store::agents)). Lower case, so a
/// library behaves the same on case-insensitive filesystems.
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

/// Every skill the library and this workspace hold, by name. Never fails: an
/// unreadable directory contributes nothing. A workspace skill replaces a
/// library one of the same name.
pub fn catalog(library: &Path, workspace: Option<&Path>) -> Vec<Skill> {
    let mut found = read_dir(library, SkillScope::Library);

    if let Some(root) = workspace {
        for mut skill in read_dir(&workspace_dir(root), SkillScope::Workspace) {
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

/// A workspace's own skills directory: `.aegis/skills/`.
pub fn workspace_dir(root: &Path) -> std::path::PathBuf {
    root.join(workspace::CABINET_DIR).join(LIBRARY_DIR)
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

        // Only a skill-named directory holding a `SKILL.md` is a skill; anything
        // else in `skills/` is somebody else's.
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

/// Reads and parses one `SKILL.md`; read and parse failures share one message
/// shape for the author.
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

/// The runbook of one catalog entry, read from disk now (PLAN 7.6, body on
/// demand), so an edit between turns is what runs.
pub fn load(skill: &Skill) -> Result<SkillDoc, String> {
    read(Path::new(&skill.path))
}

/// The entry a name resolves to.
pub fn find<'a>(catalog: &'a [Skill], name: &str) -> Option<&'a Skill> {
    catalog.iter().find(|skill| skill.name == name)
}

// ---------------------------------------------------------------------------
// Proposals (PLAN 7.13)
// ---------------------------------------------------------------------------

/// Where a proposal stands against the runbook it would become.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ProposalState {
    /// There is no `SKILL.md` beside it yet, so it can be applied.
    Pending,
    /// The `SKILL.md` beside it is this proposal, byte for byte. Still listed:
    /// the file stays until a person removes it.
    Applied,
    /// A different `SKILL.md` is there, and applying never replaces one
    /// (PLAN 7.13).
    Occupied,
}

/// One `PROPOSAL.md` in a workspace, as Settings lists it — never its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct SkillProposal {
    /// The name it would run under, which is its directory's.
    pub name: String,
    /// What the author versioned it as. Empty when it will not parse.
    pub version: String,
    /// The first paragraph of *When to use it*. Empty when it will not parse.
    pub summary: String,
    /// The tools its steps declare.
    pub tools: Vec<String>,
    /// The `PROPOSAL.md` itself.
    pub path: String,
    /// The `SKILL.md` applying it would write.
    pub target: String,
    /// Where it stands.
    pub state: ProposalState,
    /// Why it would not run, when it would not. A proposal with a problem is
    /// never applied (PLAN 7.13).
    pub problem: Option<String>,
}

/// Every proposal in this workspace, by name. Workspace only (PLAN 7.13); never
/// fails.
pub fn proposals(root: &Path) -> Vec<SkillProposal> {
    let dir = workspace_dir(root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut found: Vec<SkillProposal> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path().join(PROPOSAL_FILE);
            (is_name(&name) && path.is_file()).then(|| proposal_for(&name, &path))
        })
        .collect();

    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// One `PROPOSAL.md` as a listed proposal.
fn proposal_for(name: &str, path: &Path) -> SkillProposal {
    let target = path.with_file_name(SKILL_FILE);
    let text = fs::read(path).ok();

    let mut proposal = SkillProposal {
        name: name.to_owned(),
        version: String::new(),
        summary: String::new(),
        tools: Vec::new(),
        path: path.display().to_string(),
        target: target.display().to_string(),
        state: match (fs::read(&target), &text) {
            (Err(_), _) => ProposalState::Pending,
            (Ok(live), Some(proposed)) if &live == proposed => ProposalState::Applied,
            (Ok(_), _) => ProposalState::Occupied,
        },
        problem: None,
    };

    match read(path) {
        Ok(doc) => {
            proposal.version = doc.version;
            proposal.summary = doc.summary;
            proposal.tools = doc.tools;
        }
        Err(problem) => proposal.problem = Some(problem),
    }

    proposal
}

/// Whether a write is the apply of a proposal, and whether that apply may go.
///
/// An apply is a write of `.aegis/skills/<name>/SKILL.md` (`relative` to the
/// root) whose `content` is the `PROPOSAL.md` beside it, byte for byte — never a
/// flag the model sets.
///
/// * `None`: not an apply (any other `SKILL.md` write is PLAN 7.6's path).
/// * `Some(Ok(name))`: an apply that may be put to a person.
/// * `Some(Err(reason))`: an apply that is refused — the proposal will not
///   parse, or there is already a runbook there to replace.
pub fn apply_of(root: &Path, relative: &Path, content: &str) -> Option<Result<String, String>> {
    let mut parts = relative.components().filter_map(|part| match part {
        std::path::Component::Normal(name) => name.to_str(),
        _ => None,
    });
    let (Some(cabinet), Some(skills), Some(name), Some(file), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    // Case-folded fixed segments: `.Aegis/Skills/x/skill.md` is the same file
    // on folding filesystems.
    if !cabinet.eq_ignore_ascii_case(workspace::CABINET_DIR)
        || !skills.eq_ignore_ascii_case(LIBRARY_DIR)
        || !file.eq_ignore_ascii_case(SKILL_FILE)
        || !is_name(name)
    {
        return None;
    }

    let dir = workspace_dir(root).join(name);
    let proposed = fs::read(dir.join(PROPOSAL_FILE)).ok()?;
    if proposed != content.as_bytes() {
        return None;
    }

    if let Err(problem) = read(&dir.join(PROPOSAL_FILE)) {
        return Some(Err(format!(
            "`{name}`'s proposal will not parse, and a proposal that does not parse is never \
             applied: {problem}. Fix `{PROPOSAL_FILE}` first"
        )));
    }

    if dir.join(SKILL_FILE).exists() {
        return Some(Err(format!(
            "there is already a `{SKILL_FILE}` for `{name}`, and applying a proposal never \
             replaces a runbook — that one was not proposed through this path. Leave it as it \
             is, and say so"
        )));
    }

    Some(Ok(name.to_owned()))
}

/// Whether this workspace holds a proposal of this name, so `skill_run` can say
/// it needs applying.
pub fn is_proposed(root: &Path, name: &str) -> bool {
    is_name(name) && workspace_dir(root).join(name).join(PROPOSAL_FILE).is_file()
}

// ---------------------------------------------------------------------------
// The allow-list, and the catalog the model sees
// ---------------------------------------------------------------------------

/// The skills `agent` may run, in catalog order: granted and parseable.
pub fn granted<'a>(catalog: &'a [Skill], agent: &Agent) -> Vec<&'a Skill> {
    catalog
        .iter()
        .filter(|skill| skill.runnable() && agent.allows_skill(&skill.name))
        .collect()
}

/// The catalog block for the system message, or `None` with no skills (no
/// paragraph about an unused feature). Each line: name, when to use it, tools.
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

/// What a skill tool needs from the runtime, passed in through
/// [`ToolCtx`](crate::tools::ToolCtx) so the tools test without an app.
#[derive(Debug, Clone, Copy)]
pub struct SkillCtx<'a> {
    /// The user's library directory.
    pub library: &'a Path,
    /// The session's workspace root, when it has one.
    pub workspace: Option<&'a Path>,
    /// The tools the identity holds, for the fail-closed check.
    pub tools: &'a [String],
    /// The skill currently running, set and cleared by [`track`]; audit lines
    /// carry it (PLAN 7.6).
    pub active: Option<&'a str>,
}

impl SkillCtx<'_> {
    /// The first tool a runbook declares that the identity does not hold.
    pub fn missing<'d>(&self, declared: &'d [String]) -> Option<&'d str> {
        declared
            .iter()
            .find(|wanted| !self.tools.iter().any(|held| held == *wanted))
            .map(String::as_str)
    }
}

/// The name a skill tool reports it opened, in [`ToolResult::meta`]. Read from
/// the result, not the arguments: the tool decides.
pub const META_SKILL: &str = "skill";

/// The status a `skill_return` recorded, in [`ToolResult::meta`].
pub const META_STATUS: &str = "status";

/// The summary a `skill_return` recorded, in [`ToolResult::meta`].
pub const META_SUMMARY: &str = "summary";

/// What a run's `skill_return` reported, for a scheduled run's row (Phase 16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Returned {
    /// The runbook that returned.
    pub skill: String,
    /// `done`, `blocked` or `needs_you`.
    pub status: String,
    /// The summary, as the return carried it.
    pub summary: String,
}

/// Where a turn leaves what its last run returned; the caller lends it to the
/// turn and reads it afterwards, like [`handoff::Open`](crate::handoff::Open).
/// The mutex only gives interior mutability through a shared reference.
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

/// Reads a successful `skill_return` result as what it recorded — from the
/// result, so a refused return reports nothing.
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

/// Follows a skill run across tool calls: `skill_run` opens it, `skill_return`
/// closes it.
///
/// The name (not the body) carries across turns, since the round cap splits
/// real runs (`IDEAS.md` § 10):
/// [`TurnRegistry::carry_run`](crate::agent::registry::TurnRegistry::carry_run)
/// seeds and stores it, and a cancel or
/// [`MAX_RUN_TURNS`](crate::agent::registry::MAX_RUN_TURNS) closes it.
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

/// Creates the library and offers each seeded runbook once per name, recorded
/// in [`SEEDED_FILE`], so a deleted runbook never comes back and a new build's
/// runbook still reaches old installs. Best effort.
///
/// A library older than the manifest counts a [`SEEDED_BEFORE`] name as offered
/// only if its directory exists: wrongly assuming "offered" would lose a
/// runbook silently, while wrongly assuming "not offered" only restores an
/// example once.
///
/// The seeds are the mode's own runbooks (review, `cos.loop`, cabinet founding,
/// the four world skills) and the Phase 19 domain packs — see
/// `docs/guide/packs.md` for what each pack declares and why. Seeding grants
/// nothing (PLAN 7.6, *Authoring*).
pub fn seed(library: &Path) {
    let manifest = library.join(SEEDED_FILE);
    let mut offered: Vec<String> = match fs::read_to_string(&manifest) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        // A library from before the manifest: infer from its disk.
        Err(_) if library.is_dir() => SEEDED_BEFORE
            .iter()
            .filter(|name| library.join(name).is_dir())
            .map(|&name| (*name).to_owned())
            .collect(),
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
    // After the runbooks: a crash in between costs a redundant write, not a
    // lost runbook.
    if let Err(err) = fs::write(&manifest, format!("{}\n", offered.join("\n"))) {
        tracing::warn!(%err, path = %manifest.display(), "could not record what was seeded");
    }
    tracing::info!(dir = %library.display(), written, "example runbooks seeded");
}

/// The library's record of runbooks already offered, one name per line.
const SEEDED_FILE: &str = ".seeded";

/// What a library older than [`SEEDED_FILE`] **may** have been offered. Early
/// builds skipped existing libraries, so some never got [`COS_SKILL`]; [`seed`]
/// checks the disk.
const SEEDED_BEFORE: [&str; 2] = [REVIEW_SKILL, COS_SKILL];

/// Every runbook this build seeds, and the body each starts as.
const SEEDED: [(&str, &str); 25] = [
    (REVIEW_SKILL, REVIEW_SEED),
    (COS_SKILL, COS_SEED),
    (FOUND_SKILL, FOUND_SEED),
    (DRAFT_SKILL, DRAFT_SEED),
    (PERCEIVE_SKILL, PERCEIVE_SEED),
    (VERIFY_SKILL, VERIFY_SEED),
    (CHECK_SKILL, CHECK_SEED),
    (REVIEW_DIFF_SKILL, REVIEW_DIFF_SEED),
    (DEPLOY_SKILL, DEPLOY_SEED),
    (ALERT_SKILL, ALERT_SEED),
    (MAIL_SKILL, MAIL_SEED),
    (THREAD_SKILL, THREAD_SEED),
    (REPLY_SKILL, REPLY_SEED),
    (WATCH_SWEEP_SKILL, WATCH_SWEEP_SEED),
    (WATCH_DIGEST_SKILL, WATCH_DIGEST_SEED),
    (WATCH_IMPACT_SKILL, WATCH_IMPACT_SEED),
    (BUDGET_POSITION_SKILL, BUDGET_POSITION_SEED),
    (BUDGET_RUNWAY_SKILL, BUDGET_RUNWAY_SEED),
    (BUDGET_ALERT_SKILL, BUDGET_ALERT_SEED),
    (SOCIAL_SCAN_SKILL, SOCIAL_SCAN_SEED),
    (SOCIAL_REPLY_SKILL, SOCIAL_REPLY_SEED),
    (SOCIAL_POST_SKILL, SOCIAL_POST_SEED),
    (WISH_LIST_SKILL, WISH_LIST_SEED),
    (REVENUE_THESIS_SKILL, REVENUE_THESIS_SEED),
    (REVENUE_PIPELINE_SKILL, REVENUE_PIPELINE_SEED),
];

/// The standing rule of the whole mode, as a runbook.
pub const REVIEW_SKILL: &str = "never-send-without-review";

/// `never-send-without-review`: the mode's standing rule, and an example that
/// needs no connector (PLAN 7.6, *Verifier is a skill*).
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

/// `cos.loop`: `COS.md` *Loop* as a runbook rather than prompt text
/// (PLAN 7.1), so it costs one catalog line and its owner can edit it.
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

/// Founding a cabinet (PLAN 7.14).
pub const FOUND_SKILL: &str = "cabinet.found";

/// `cabinet.found`: writes `.aegis/roster/PROPOSAL.md` and creates nobody; a
/// person applies it in Settings ([`roster`](crate::roster)). Its steps carry
/// the packs' declared tools so proposed grants do not fail closed; its example
/// is indented so a `## ` is not read as a section by [`doc::parse`].
pub const FOUND_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# cabinet.found

## When to use it

When someone asks for a cabinet in this project — a Chief of Staff, a reviewer,
the specialists the work needs — and the team does not exist yet or lacks a
role. You write one proposal file and create nobody: a person applies it in
Settings, and applying it is the grant.

Do not run it to change an identity that already exists. Apply skips every name
already on file and never widens one; a wider Reviewer is an edit a person makes
by hand.

## Inputs required and tools it will call

- What the human said in this conversation about three things: which domains
  this cabinet is for (delivery, intake, watch, budget, social, revenue and the
  wish list, or none beyond routing); whether this project has a world, an
  essence written down in `world/`; and whether anything should ever run while
  nobody is watching.
- `.aegis/`, which must already exist. The shared files are laid down by a
  button in the project panel, not by you.
- `.aegis/roster/PROPOSAL.md`, when a roster was proposed before.

Calls `fs_list` to see what the cabinet holds, `fs_read` to read an earlier
proposal, and `fs_write` once, for the proposal. Nothing else: no tool creates
an identity, a routine or a connector, and this runbook does not look for one.

## Steps

1. If the human has not answered all three questions above, ask them and stop.
   Do not infer a domain from the folder: a `package.json` is not a request for
   a delivery team.
2. `fs_list` `.aegis/`. If it is not there, stop — see the last heading.
3. If `.aegis/roster/PROPOSAL.md` exists, `fs_read` it and start from it. Keep
   what it proposes and add only what the answers call for.
4. Propose the two identities every cabinet gets, as in the example below, then
   one specialist per domain the human named, and no other. The human is not a
   row. The built-in Assistant is not a row either; apply never touches it.
5. The Chief of Staff routes. It never holds `shell_exec`, `screen_capture` or
   `handoff_return`, and it is never on a clock: `runs_per_day: 0`, and no
   intended routine names it. A Chief that runs programs is doing the work.
6. The Reviewer reads and never writes: `fs_list, fs_read, skill_run,
   skill_return`, and no `fs_write`, `shell_exec` or `handoff_delegate`. Grant
   it only runbooks whose declared tools it holds — `world.check` when this
   project has a world, otherwise `none`. `review.diff` calls `fs_write` and
   `shell_exec`: when delivery was asked for, it goes to the Delivery
   specialist, and an open question says the Reviewer cannot run it as proposed.
7. A specialist holds its pack's runbooks, the tools they declare, and
   `skill_run, skill_return`. Declared tools, by pack:
   - Delivery: `review.diff, deploy.draft, alert.draft` call `fs_list, fs_read,
     fs_write, shell_exec`.
   - Intake: `mail.triage, thread.recap, reply.draft` call `fs_list, fs_read,
     fs_write`.
   - Watch: `watch.sweep, watch.digest, watch.impact` call `fs_list, fs_read,
     fs_write`.
   - Budget: `budget.position, budget.runway, budget.alert` call `fs_list,
     fs_read, fs_write`.
   - Social: `social.scan, social.reply, social.post` call `fs_list, fs_read,
     fs_write`.
   - Revenue: `wish.list, revenue.thesis, revenue.pipeline` call `fs_list,
     fs_read, fs_write`.
   `runs_per_day` is 0 unless the human said that domain runs unattended. Then
   it is a small number, and the clock goes under Intended routines as one line:
   who, which runbook, how often. Never `cos.loop`. A routine only exists once
   a person has watched that runbook run and saves it in Settings.
8. A connector the work needs (a mailbox, a monitor, a broker) is not a tool
   name in the roster. Put it under Open questions: installing a program is the
   operator's, and apply refuses a connector tool nothing answers to.
9. If the human said this project has an essence and there is no `world/`, name
   `world.draft` under Open questions. Do not write `world/`.
10. `fs_write` `.aegis/roster/PROPOSAL.md` whole, in this shape. Every identity
    has the four fields on dash lines; lists are comma-separated, and `none` is
    an empty one. A line without a dash is prose for the reader and grants
    nothing.

    # Roster

    What this cabinet is for, in the human's words.

    ## Chief of Staff

    - role: routes work to specialists, keeps the board, and asks the human only when it must
    - tools: fs_list, fs_read, fs_write, skill_run, skill_return, handoff_delegate, memory_write, memory_search
    - skills: cos.loop, never-send-without-review, world.check, world.perceive-delta
    - runs_per_day: 0

    ## Reviewer

    - role: reads what the cabinet produced and says what is wrong with it, without changing it
    - tools: fs_list, fs_read, skill_run, skill_return
    - skills: none
    - runs_per_day: 0

    ## Intended routines

    - none

    ## Open questions

    - none

## How to validate

`fs_read` the file back. Every heading but the last two is an identity with
exactly role, tools, skills and runs_per_day. The Chief holds no `shell_exec`
and has `runs_per_day: 0`. No identity holds a runbook whose declared tools it
lacks. Every specialist is for a domain the human named. Settings lists the
proposal with the reason when it will not parse, and nothing is created until a
person applies it.

## What to return

`skill_return` with `status: done`, `.aegis/roster/PROPOSAL.md` in `artefacts`,
and a summary naming each identity proposed and saying that none exists until
the proposal is applied in Settings → Identities. Copy the file's open questions
into `open_questions`. `status: needs_you` when one of the three questions is
still unanswered, with it in `open_questions`.

## What requires approval

The one `fs_write` is put to the human, and its preview is the first time they
see the team. Applying is not a step here and has no tool: it is a person
pressing apply in Settings, and that press is the grant. Never write
`agents.json`, `routines.json`, `connectors.json`, a `SKILL.md` or anything
under `world/`, and never propose a grant for yourself.

## What to do if the source is missing

If `.aegis/` is not there, return `status: blocked` and say that the shared
files are missing and that *Set up shared files* in the project panel lays them
down. Do not create `.aegis/roster/` with the write: a roster in a workspace
nobody set up is a team for a project that has not agreed to have one.
"#;

/// Help with founding or amending a world (PLAN 7.2).
pub const DRAFT_SKILL: &str = "world.draft";

/// `world.draft`: the only seeded runbook that writes `world/`, for an attended
/// session (refused inside a brief, PLAN 7.2). It drafts the descriptive files
/// from evidence and never invents the essence or the oracle.
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

/// `world.perceive-delta`: reads only the declared sources that moved and
/// returns a proposal; never writes `world/`.
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

/// `world.verify`: checks an instance against the oracle, with paths as
/// evidence (`COS.md` *Work*).
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

/// `world.check`: a cheap, read-only pass over the constitution before touching
/// a world.
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

/// `review.diff`: reads a range via `git` and asks the same four questions in a
/// fixed order, reading surrounding files, not just hunks.
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

/// `deploy.draft`: everything up to the deploy, which stays human (PLAN 7.4).
/// Facts come from the project's own files; without them it is `blocked`.
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

/// `alert.draft`: an incident note separating observed (with commands) from
/// inferred, plus an unsent client reply carrying no guessed cause. Never
/// restarts anything.
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

// ---------------------------------------------------------------------------
// The client-intake pack (PLAN 7.3, Phase 19, pack 2)
// ---------------------------------------------------------------------------

/// Turn one inbound message into a ticket (PLAN 7.3, Phase 19: *mail first —
/// read + draft, never send*).
pub const MAIL_SKILL: &str = "mail.triage";

/// `mail.triage`: one message file into a ticket. The ask must be a quoted,
/// dated sentence so *no ask* is possible; money or access requests are
/// `needs_you`. Step 1 handles `.eml` attachments, which can fill or overflow
/// [`READ_MAX_BYTES`](crate::tools::READ_MAX_BYTES).
const MAIL_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# mail.triage

## When to use it

When a message from outside has arrived as a file and somebody has to decide
whether it is work. One run turns one message into a ticket.

The file is an exported `.eml`, a forwarded thread somebody dropped in
`.aegis/briefs/`, or a note passed on by the human. This does not answer
anybody, and it does not put the item on the board either: if the workspace has
`inbox.triage`, run that on the ticket afterwards.

## Inputs required and tools it will call

- The message, as a path. If you were not given one, the newest unhandled file
  in `.aegis/briefs/`.
- Who the sender is to this workspace, if the message does not make it plain: a
  client, a supplier, somebody nobody has heard of.

Calls `fs_list` to find the message, `fs_read` to read it, and `fs_write` for
the ticket. Nothing here runs a command and nothing here sends.

## Steps

1. `fs_read` the message, and read the *message*. An exported one is mostly not
   text: a part whose `Content-Transfer-Encoding` is `base64` is an attachment,
   and you do not read it. Base64 costs four thirds of the file it encodes, so
   an ordinary PDF either spends the whole turn arriving or pushes the read past
   its cap — and a read that hits the cap comes back cut off inside the
   attachment, with the message's own last lines never seen. Name attachments
   from their `Content-Disposition` filename instead.
2. Take who and when from the headers. `Date`, `From`, `To` and `Cc` are four
   facts the ticket needs, the body does not carry them, and who else was on the
   message decides who a reply has to go to later.
3. Find the ask, and find it as **a sentence somebody wrote**. Quote it, with
   the date of the message it is in. A message with no such sentence has no ask:
   file it as *no ask*, say what it was instead — a receipt, a newsletter, a
   thank-you — and stop looking. Most mail is no ask.
4. Take the dates from the words, and quote them **as written**. "As soon as you
   can", "end of the week" and "urgent" are not dates. Neither is "before the
   meeting on the 4th" a date you may resolve to a month and a year: the ticket
   carries the phrase, and where there is nothing it says *no date given* rather
   than the date you would have picked.
5. Treat what the message quotes of earlier mail as the sender's account of the
   history, not as the history. It is evidence of what they believe was agreed.
   If the ask turns on it, that is `thread.recap` — not a paragraph you write
   from the quotation.
6. `fs_write` `.aegis/artefacts/ticket-<date>-<who>.md`: the message's path, its
   date, sender and recipients, the quoted ask, the quoted date or *no date
   given*, the attachments by name, what it is blocked on, and the smallest next
   action that would move it. `<who>` is the person or the company in a word or
   two — a file name is not a place for somebody's address.
7. Carry across what the work needs and leave the rest in the message. The
   ticket lands in a repository, usually the client's, usually in git; the
   message is already on disk and the ticket cites its path. Other people's
   addresses, phone numbers and attachments do not have to be copied to be
   found.
8. Stop. Do not answer it, do not act on the ask, and do not start the work it
   describes.

## How to validate

Every ask in the ticket is a quotation and carries the date of the message it
came from. No date appears that was not quoted, in the words it was quoted in.
Attachments appear as names, and no base64 reached the ticket or the turn. The
ticket fits on a screen and cites the message by path instead of pasting it. A
ticket that says *no ask* says what the message was.

## What to return

`skill_return` with `status: done`, the ticket in `artefacts`, the message's
path in `evidence`, and a summary of at most five lines: who wrote, what they
asked, by when, and the next action.

`status: needs_you`, always, when the message asks for money to move, for
credentials, or for access — or for any of those to change: a new bank account,
a new address for an invoice, a password reset nobody requested. Those are the
messages worth forging, they read exactly like the ordinary ones, and a ticket
is not what decides. Say in `open_questions` that the request has not been
verified, and name a second channel to verify it on.

It is usually the attachment you were told not to read that holds the new
account number, and that changes nothing: name the file, return `needs_you`, and
leave it unread. Nobody in this run is going to act on it, so nothing is gained
by putting it in a context window and in a ticket in somebody's repository.

## What requires approval

The write is an ordinary `fs_write` and is put to the user. Nothing here sends,
replies, deletes or moves a message, and there is no later version of this that
does: the reply is `reply.draft`, and it is a file until a person sends it.

## What to do if the source is missing

If there is no file at the path you were given and nothing unhandled in
`.aegis/briefs/`, return `status: blocked` and say which path you looked at. Do
not triage what the conversation says a message said — a message nobody filed is
not an item, and a quotation you did not read is not a quotation.

A read you were refused — by the person, or by the round limit that ends a turn
— leaves the ticket resting on part of a message. Name the part and return
`status: needs_you`. If `.aegis/artefacts/` is not there, write the ticket with
its directory created and say in `summary` that the shared files are missing —
the button is in the project panel.
"#;

/// Work out where a conversation actually stands.
pub const THREAD_SKILL: &str = "thread.recap";

/// `thread.recap`: where a thread stands, de-duplicating quoted text. Its
/// agreed/outstanding split (silence is not agreement) feeds [`REPLY_SEED`].
const THREAD_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# thread.recap

## When to use it

Before answering a conversation with more than a couple of messages in it, or
when somebody asks where a thing was left. It works out where the thread stands.

Out of it come three lists: what was agreed, what is outstanding, and what was
asked and never answered. One run recaps one thread, and it recaps a
conversation rather than a relationship — a question this thread does not answer
is a question the recap names, not a reason to go and read the others.

## Inputs required and tools it will call

- The thread, as a path: a directory of messages, an export, or a single file
  with the conversation in it. `fs_list` what you were given first, so the recap
  covers the messages that are there rather than the ones that were mentioned.
- Which side you are. A recap that does not know who "we" is cannot say who owes
  what.

Calls `fs_list` and `fs_read` for the messages and `fs_write` for the recap.
Nothing here runs a command and nothing here answers the thread.

## Steps

1. Read the messages **oldest first**, in the order they were sent. The last
   message is not the state of the thread. A commitment lives where it was made,
   which is usually in the middle, and reading backwards finds the version
   somebody restated instead of the one they made.
2. Count each sentence once. The thread is quoted into every reply, so one
   promise appears eight times over; attribute it to the message that first
   carried it and skip it everywhere else. Eight copies of one commitment read
   as eight commitments, and that is what makes a thread look like a project.
3. Usually the thread is one file and the older messages exist only as the
   quotations inside it. Then say so: those lines are the quoter's copy, pasted
   by somebody with a position, and a mail client trims what it quotes. Mark
   them **as quoted by** whoever forwarded them, and treat a claim that survives
   only in a quotation as weaker than one in a message you have.
4. Sort every claim into exactly one of three. **Agreed**: one side proposed it
   and the other answered yes. **Outstanding**: proposed and not answered, or
   promised and not delivered — with who owes it and since when. **Never
   answered**: a question somebody asked that no later message addresses.
5. Silence is not agreement. A proposal nobody replied to is outstanding, and it
   stays outstanding however reasonable it was and however long ago it was sent.
6. `fs_write` `.aegis/artefacts/recap-<thread>.md`: one line per message — date,
   sender, what changed — then the three lists. Every line in them cites the
   date and sender of the message it came from. `<thread>` is the subject with
   the `Re:` chain taken off, so a conversation gets one recap rather than one
   per round of it.
7. Stop. The recap is what a reply gets written from; it is not a reply, and it
   decides nothing on the outstanding list.

## How to validate

Every line of the three lists cites one message by date and sender. Nothing
appears in two lists. *Agreed* holds only claims with two messages behind them,
the proposal and the answer. The recap ends by saying how many messages it read
and which was the last, so a reader can tell whether it is still current.

## What to return

`skill_return` with `status: done`, the recap in `artefacts`, the number and
range of messages read in `evidence`, and a summary of at most five lines: what
is agreed, what is outstanding, and who is waiting on whom.

`status: needs_you` when the thread turns on something said somewhere else — a
call, a meeting, a message in another channel — with it in `open_questions`.
What was agreed on a call is not in the thread, and a recap that fills that in
is worse than one that says the thread does not contain it.

## What requires approval

The write is an ordinary `fs_write`, put to the user. Reads inside the workspace
happen without asking; a thread stored outside it is a path the person is asked
about. Nothing here replies, forwards, or files anything with the sender.

## What to do if the source is missing

If the path holds no messages, return `status: blocked` and say what you listed.
Do not recap a conversation from the ticket about it, or from what the session
said it contained: a recap is a reading of the messages, or it is a rumour with
dates on it.

If you could read only some of them — a refusal, a format you cannot open, the
round limit that ends a turn — the recap covers those and says so in its first
line, and you return `status: needs_you` when what is missing is where the
answer would be.
"#;

/// Draft the answer somebody else sends (`COS.md` *Loop*; PLAN 7.4).
pub const REPLY_SKILL: &str = "reply.draft";

/// `reply.draft`: a reply a person sends (PLAN 7.4). Every date, price and
/// scope cites its file; an unsourced commitment goes to `open_questions`. It
/// never picks its own input.
const REPLY_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write
---

# reply.draft

## When to use it

When somebody outside is owed an answer and a person will send it. The reply is
written as a file, with a source named under every claim in it.

It does not send, and there is no later version of it that does.

## Inputs required and tools it will call

- The ticket, as a path — the one `mail.triage` wrote. If you were not given
  one, that is the end of the run. A reply drafted to whatever was written most
  recently is how the wrong client gets answered, and it will read perfectly.
- The recap, if the thread has one, and the workspace's
  `.aegis/status/STATUS.md` and `.aegis/decisions/DECISIONS.md`. Those are where
  a commitment is allowed to come from.

Calls `fs_read` for those and `fs_write` for the draft. It lists nothing, runs
nothing, and sends nothing.

## Steps

1. `fs_read` the ticket, then the recap if there is one. Answer the ask the
   ticket quotes — not the question you would rather they had asked, and not the
   four other things you noticed on the way past.
2. Before writing a word, decide what the reply commits to. A date, a price, a
   scope, a name, an order of work: each is a commitment, and each needs a file
   behind it — the decisions ledger, the board, a plan `deploy.draft` wrote, or
   the ticket's own quotation.
3. A commitment with nothing behind it does not get softer wording. "I'll look
   into it", "should be fine", "early next week" are commitments in the reader's
   hands, and the reader is right. Leave it out of the draft and put it in
   `open_questions`.
4. Write the draft: who it is to, the subject, the body. Short, in the language
   the message was written in, answering the quoted ask in its first two lines.
   It goes to everyone the message went to — the ticket lists them — because
   taking somebody off a thread is a decision about who gets to see the answer,
   and it is not one this runbook makes on the way past.
5. Under the body, a **sources** block: one line per claim in the reply, naming
   the file it came from. It is not part of the message — it is what the person
   reviewing reads instead of reconstructing the reply from scratch.
6. `fs_write` it beside the ticket, with `.reply` before the extension:
   `.aegis/artefacts/ticket-<date>-<who>.reply.md`.
7. Stop. Sending is the human's, after `never-send-without-review` — this draft
   is what that runbook was seeded for, so run one into the other. Do not send
   it, do not schedule it, and do not tell anybody it is on its way.

## How to validate

Every sentence of the body either answers the quoted ask or has a line in the
sources block. No date, price or scope is in the reply that is not in a file the
sources block names. The draft answers one message; if it answers two, it is two
drafts.

## What to return

`skill_return` with `status: done`, the draft in `artefacts`, the ticket and the
files behind the sources block in `evidence`, and a summary of at most five
lines: what the reply says and what it commits to.

`status: needs_you` when the answer turns on something that is not in a file — a
price, a deadline, whether to take the work at all — with it in `open_questions`
and the draft left unwritten. A draft that guesses at the price is a draft
somebody sends.

## What requires approval

The write is an ordinary `fs_write`, put to the user. There is no tool here that
sends, and a mail connector installed later does not change that: a connector
replaces where a message comes from, not the gate it goes out through
(`PLAN.md` § 7.6). A reply is a file until a person sends it.

## What to do if the source is missing

No ticket, no run: return `status: blocked` and ask for the path. Do not draft
from the session's account of what the client wrote. The quotation is the whole
point, and a reply written from a summary of a message is a reply to a message
that does not exist.

If the ledger or the board is missing, say so and draft only what the ticket
supports — a workspace where nothing has been decided in writing is a fact the
summary should carry. A read you were refused, by the person or by the round
limit that ends a turn, is a source the block cannot name: leave the claim out
and return `status: needs_you`.
"#;

/// Turn what arrived into entries (PLAN 7.3, Phase 19, pack 3).
pub const WATCH_SWEEP_SKILL: &str = "watch.sweep";

/// `watch.sweep`: files in the workspace into entries that keep *what it says*
/// apart from *what it shows*. What was swept is the set of entry files; delete
/// one to re-read it.
const WATCH_SWEEP_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# watch.sweep

## When to use it

When material for the watch has arrived as files and nothing has read it yet.
One run turns what is new in one folder into one entry per item.

The material is whatever you put there: a saved page, a release note, a paper,
an export, a note somebody passed on. There is no tool here that fetches
anything, and nothing in this run reaches the network.

## Inputs required and tools it will call

- The folder the material is in — `.aegis/briefs/` unless you were given
  another. `fs_list` it first, so the sweep covers what is on disk rather than
  what was mentioned.
- Nothing else. What has already been swept is answered by the entry names in
  `.aegis/artefacts/`, not by anybody's memory of last time.

Calls `fs_list` for both folders, `fs_read` for the material, and `fs_write`
for one entry per new item. Nothing here runs a command and nothing here
fetches.

## Steps

1. `fs_list` `.aegis/artefacts/` and read the names. An entry is
   `watch-<source>.md`, where `<source>` is the material's file name without its
   extension. A source already named there has been swept: skip it without
   reading it. That is the whole of the bookkeeping, and it is names rather than
   a ledger so that deleting an entry is how you ask for an item to be read
   again. Where the name is taken by a different file, put the folder in it too.
2. `fs_list` the material folder and take what is left. If nothing is left, the
   sweep found nothing — write no entries and go to *What to return*. That is
   the answer most days.
3. For each new item, `fs_read` it and write down two things separately: **what
   it says** and **what it shows**. A claim is what the source asserts — faster,
   cheaper, the first, the only. Evidence is what somebody else could go and
   check: a number with its method beside it, a repository, a licence, a price,
   a date, a name. Most announcements are all claim. Say so; that is not a
   criticism of the item, it is the fact the entry exists to carry.
4. Quote the claim rather than restating it, and take the date from the source.
   A date inferred from where the file sits is not a date — write *undated*
   instead. An undated source is worth less than a dated one, and a reader of
   the entry should be able to see that.
5. Say who published it and what they sell. A benchmark in a launch post, a
   forecast from somebody holding the position, a study funded by the thing it
   measures: none of that is disqualifying, and all of it belongs in the entry.
6. `fs_write` `.aegis/artefacts/watch-<source>.md` for each: the source's path,
   its date or *undated*, who published it, the quoted claim, what is shown
   behind it, and one line on what it would touch here — a file, a dependency,
   a cost, or nothing. One entry per item, however tempting a combined one is:
   two items in one file is one item that cannot be skipped later.
7. Stop. Do not work out whether any of it matters. That is `watch.impact`, on
   one entry, and it needs this file written first.

## How to validate

Every entry names the file it came from, and nothing else in it claims to be a
source. Every claim is a quotation. No entry carries a date its source did not.
An entry whose source shows nothing says so in those words, rather than
paraphrasing the claim into something that sounds checked.

## What to return

`skill_return` with `status: done`, the entries in `artefacts`, the material's
paths in `evidence`, and a summary of at most five lines: how many items were
new, what they were, and which of them showed anything.

*Nothing new* is a complete run: `status: done`, no artefacts, one line naming
the folder you listed and what was already swept. A sweep that finds something
every time is a sweep reading the same page twice.

`status: needs_you` when a source is somebody's private material — a client's
document, a contract, a message — dropped in the watch folder by mistake. Name
the file, do not enter it, and do not summarize it in the return either.

## What requires approval

The writes are ordinary `fs_write` calls. Reads inside the workspace happen
without asking, and a folder outside it is a path the person is asked about
every time — which is why the material belongs in the workspace before a sweep,
and certainly before one on a clock. Nothing here fetches, downloads,
subscribes, or answers anybody.

## What to do if the source is missing

If the material folder is not there, or holds nothing at all, return
`status: blocked` and say which folder you listed. Do not sweep from what the
session says has been happening: a watch whose entries came out of a
conversation is a watch reporting its own memory back to you.

A read you were refused — by the person, or by the round limit that ends a turn
— leaves that item unswept, and unswept is where it should stay. Write the
entries you could, name the ones you could not in `open_questions`, and return
`status: needs_you`. Half an entry carrying the source's own headline is worse
than no entry.
"#;

/// Report what is new since the last report (PLAN 7.3, Phase 19, pack 3).
pub const WATCH_DIGEST_SKILL: &str = "watch.digest";

/// `watch.digest`: written for a clock. A quiet period writes nothing and
/// returns `done`, not `blocked`, so the routine does not pause itself
/// (PLAN 7.6). Each digest lists the entries it covered, so the next run reads
/// only what is new.
const WATCH_DIGEST_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# watch.digest

## When to use it

When the watch is due to report: a week, a morning, a clock. It reads the
entries written since the last digest and reports only what that one did not.

This is the runbook of the pack that belongs on a clock, and the only one here
written for a reader who was not in the room.

## Inputs required and tools it will call

- Nothing. `fs_list` `.aegis/artefacts/` and the run has what it needs: the
  entries `watch.sweep` wrote, and the digests written before this one.
- The period, if you were given one. Without one the period is *since the last
  digest*, which is what that digest's own closing list answers.

Calls `fs_list` and `fs_read` in `.aegis/artefacts/`, and `fs_write` for the
digest — when there is one to write.

## Steps

1. `fs_list` `.aegis/artefacts/`. Digests are `watch-digest-<date>.md` and
   entries are `watch-<source>.md`. `fs_read` the newest digest **first** and
   read the list of entry names at the end of it. That list is where the watch
   got to, and it is the only thing that keeps this run cheap.
2. Take the entries that list does not name. Those are the run. Do not re-read
   the ones it does, and do not open the sources any of them came from: an entry
   is what its source was read into, and reading both is paying twice for one
   item.
3. If nothing is left, write no file. *Nothing new since <date>* is the answer,
   and it is the answer most of the time. One digest per empty week is a folder
   nobody opens and a watch nobody believes.
4. Sort what is left into **changed**, **worth reading** and **noise**, each item
   in exactly one. *Changed* is something now true that was not — a version
   shipped, a price moved, a licence changed, a company bought. *Worth reading*
   is an argument somebody here would be better for having read. *Noise* is the
   rest, one line each, because "eleven of these arrived" is information and
   eleven summaries of them are not.
5. Cap the first two lists at five items each. Past five, the sixth is noise by
   the definition above, whatever it is about. A digest that lists everything has
   handed the sorting back to the reader, which was the work.
6. Every line names the entry it came from — the entry, not the source, because
   the entry names the source and carries what was actually shown. A digest
   citing a page directly is a digest whose claims cannot be checked without
   leaving the folder.
7. `fs_write` `.aegis/artefacts/watch-digest-<date>.md`: the period covered, the
   three lists, and — last — every entry name this digest looked at, including
   the ones it filed as noise. That closing list is what the next run reads
   first, so an entry left out of it is an entry reported twice.
8. Stop. A digest reports; it decides nothing. An item in it that looks like it
   changes what this project does is one run of `watch.impact` on that entry,
   started by somebody.

## How to validate

Every line of the digest names an entry file. No entry is in two lists. Nothing
in it appeared in the previous digest. The closing list holds every entry the
run looked at, so its length is the number of entries considered. The digest
fits on a screen.

## What to return

`skill_return` with `status: done`, the digest in `artefacts`, the entry names
in `evidence`, and a summary of at most five lines: the period, what changed,
and how many items were noise.

An empty period is also `status: done`: no artefact, and one line saying nothing
is new since the last digest and which digest that was. A quiet week is this
runbook working, not failing — `blocked` would put a routine two silences from
pausing itself over it.

`status: needs_you` for one thing only: an entry saying something has happened
to something this project currently relies on — a dependency abandoned, a
licence changed under it, a service closing. That is not a line to be read on
Friday.

## What requires approval

One `fs_write` inside the workspace, and nothing else. That matters more here
than in the rest of the pack, because this is the runbook meant to fire
unattended, and an unattended run is never asked anything: what it may do beyond
reading is exactly what was signed on the routine, and everything else is
refused rather than put to somebody. A version of this that wanted a folder
outside the workspace, or a command, would be a routine that failed every
morning at the same time.

## What to do if the source is missing

If `.aegis/artefacts/` holds no entries at all, return `status: blocked` and say
so: there is nothing to digest, and the thing to run is `watch.sweep`. If it
holds entries and no digest, this is the first one — cover everything, and say
that in its first line.

If some entries could not be read — a refusal, the round limit that ends a turn
— the digest covers the ones you read, says which it could not in its first
line, and leaves those out of the closing list so the next run picks them up.
Return `status: needs_you`.
"#;

/// What one entry would mean here (PLAN 7.3, Phase 19, pack 3).
pub const WATCH_IMPACT_SKILL: &str = "watch.impact";

/// `watch.impact`: what one given entry would mean here, as conditions to check,
/// with the cost of doing nothing too. A needed `world/` change is reported as
/// an écart, never made ([`Grant::WorldAmend`] is refused to routines).
///
/// [`Grant::WorldAmend`]: crate::policy::Grant::WorldAmend
const WATCH_IMPACT_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write
---

# watch.impact

## When to use it

When one thing in the watch looks like it might change what this project does.
One run takes one entry and says what would have to be true here.

It decides nothing and recommends nothing. What comes out is what would have to
change and what that would cost, which is what a person needs in order to
decide.

## Inputs required and tools it will call

- The entry, as a path — one `watch.sweep` wrote. If you were not given one,
  that is the end of the run. A note written about whatever looked most
  interesting is a note about the wrong thing, and it will read well.
- This project's own account of itself: `world/essence.md` if there is one, the
  decisions ledger, the board. Those are what "here" means, and with none of
  them this run has nothing to compare against.

Calls `fs_read` for those and `fs_write` for the note. It lists nothing, runs
nothing, and changes nothing about the project.

## Steps

1. `fs_read` the entry. Work from what it recorded as **shown**, not from what
   it quoted as claimed. A claim is a reason to look; it is not a fact about
   this project's options.
2. `fs_read` this project's account of itself, before writing a word. What the
   project is, what it has already decided, and what it is doing now are three
   different files, and a note written without them is a description of the item
   with our name pasted on it.
3. Name what here it touches, as paths: a dependency in a manifest, a decision
   in the ledger, a constraint in the essence, a bill somebody pays monthly. If
   you cannot name a file, the honest answer is that it touches nothing here,
   and that is a good thing for a watch to produce.
4. Write the condition, not the conclusion: **what would have to be true** for
   this to be worth doing. A number nobody has, a version that has not shipped,
   a licence somebody would have to accept, a migration nobody has costed. Each
   of those is something a person could go and find out.
5. Cost it in the units this project actually pays in — files that would be
   rewritten, a dependency added or dropped, an interface other people depend
   on, money per month — and cost doing nothing beside it. Never "a moderate
   effort".
6. If what it touches is `world/`, stop there and say so. That the constitution
   would have to change is the most useful thing this note can conclude and the
   one thing it must not act on: it is an écart, it belongs to a person, and no
   run of this writes `world/` — least of all one on a clock, which is not
   offered that approval at all.
7. `fs_write` `.aegis/artefacts/impact-<entry>.md`: the entry, what it touches
   by path, what would have to be true, what the change would cost, and what
   doing nothing would cost. No recommendation, and no order of work.
8. Stop. The decision is somebody's, and whoever takes it files it in the
   decisions ledger — not this run, and not as a suggestion phrased as one.

## How to validate

Every "it touches" line names a path in this project. Every condition is
something that could be found out rather than judged. No sentence in the note
recommends a course of action. The cost of doing nothing is there, because a
note that prices only the change is an argument for the change.

## What to return

`skill_return` with `status: done`, the note in `artefacts`, the entry and the
project files you read in `evidence`, and a summary of at most five lines: what
it touches, what would have to be true, and what it would cost.

A note saying it touches nothing here is a good run, and `done`. Most of what a
watch turns up touches nothing here; a note finding consequences in every item
is the sweep manufacturing work at the other end of the pack.

`status: needs_you` when the answer turns on a decision rather than a fact —
whether to take the cost, whether the constraint still holds, whether this is
the year for it — with the question in `open_questions` and no recommendation
attached to it. And always when the essence would have to change: that is an
écart, and it is the human's.

## What requires approval

One `fs_write` into `.aegis/artefacts/`, under the usual gate. A write into
`world/` is refused outright however clearly this note argues for it — amending
the constitution is a human decision (`PLAN.md` § 7.4), and a scheduled run is
never offered that approval.

## What to do if the source is missing

No entry, no run: return `status: blocked` and ask for the path. Do not take the
newest entry, and do not work from the digest — a digest line is a pointer, and
a note written from a pointer is written from a summary of a summary of a page.

If the project has no `world/`, no ledger and no board, say so and write only
what the manifest and the files on disk support: a project that has not written
down what it is is itself worth a line in the note. A read you were refused, by
the person or by the round limit that ends a turn, is a comparison you did not
make — leave that line out and return `status: needs_you`.
"#;

/// What is held and what is owed (PLAN 7.3, Phase 19, pack 4).
pub const BUDGET_POSITION_SKILL: &str = "budget.position";

/// `budget.position`: what is held and owed. Every figure is copied from a
/// source line or shown as redoable arithmetic reconciled to stated totals, and
/// the file leads with its stalest as-of date.
const BUDGET_POSITION_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# budget.position

## When to use it

When exports have arrived and somebody needs one page saying what is held and
what is owed. One run turns them into one position file.

The exports are files you put there: a bank CSV, a broker statement, an invoice
ledger, a spreadsheet saved as text. Nothing here connects to an account, and
nothing here places an order — this is surveillance, and Aegis is not a broker.

## Inputs required and tools it will call

- The folder the exports are in — `.aegis/briefs/` unless you were given
  another. `fs_list` it first, so the position covers what is on disk.
- Which currency the position is written in, if more than one appears. Without
  one, keep the currencies apart rather than picking.

Calls `fs_list`, `fs_read` for the exports, and `fs_write` for the position
file. Nothing here runs a program, reaches an account, or trades.

## Steps

1. `fs_list` the folder and `fs_read` each export. For each, find its **as-of
   date** — the statement date, the export timestamp, the last row's date — and
   write it down before reading a figure out of it. An export with no date in it
   is dated *unknown*, which is a fact about the position and not a gap to fill
   with the file's modification time.
2. Copy figures; do not restate them. Every line of the position carries the
   number as the export wrote it, the export's file name, and where in it —
   an account, a row, a label. A figure that cannot name where it came from does
   not go in the file.
3. Two exports usually overlap, and the same transaction in two files is one
   transaction. Match on the three things that identify it — account, date,
   amount — and where two lines match on all three, take one and say which file
   you took it from. Where they nearly match, take neither and list it as a
   discrepancy.
4. Do the arithmetic **in the open**. A total appears with its addends beside
   it, so a reader can redo it. Then reconcile: if the export states its own
   total, compare yours to it and put both in the file. If they differ, the
   difference goes in the file as a number, and this run returns `needs_you`.
   Do not round the difference away and do not adjust a line to make it close.
5. Do not add across currencies unless you were given a rate and the date of
   that rate, and then say both on the line where you used them. Two currencies
   summed at a rate nobody named is a number that looks like money and is not.
6. Separate what is **held** from what is **owed** and from what is
   **committed** — money that exists, money somebody else is owed, and money
   already spoken for by a standing commitment. Anything you cannot place in one
   of the three goes in a fourth list called *unplaced*, with its source.
7. `fs_write` `.aegis/artefacts/position-<date>.md`. Its first line is the
   **stalest** as-of date among the sources, not today's, because that is how
   current the position actually is. Then the four lists, then the
   reconciliation, then the exports by file name and date.
8. Stop. Do not act on any of it, do not propose a trade, do not cancel
   anything, and do not tell anybody what to buy.

## How to validate

Every figure names the export and the place in it that it came from. Every total
shows its addends. Every reconciliation shows both numbers and their difference.
No figure appears without a date. Nothing is summed across currencies without a
named rate and the date of that rate. The first line of the file is the oldest
as-of date in it.

## What to return

`skill_return` with `status: done`, the position file in `artefacts`, the export
paths in `evidence`, and a summary of at most five lines: what is held, what is
owed, as of when, and what did not reconcile.

`status: needs_you` whenever a total does not reconcile, whenever two exports
disagree about the same transaction, and whenever an export carries no date. All
three are the same fact — the position rests on something that has to be looked
at by a person — and a position file that quietly picked one side of any of them
is worse than one that stops.

## What requires approval

One `fs_write` inside the workspace. There is no tool here that reaches an
account, moves money, or places an order, and a connector installed later does
not change that: a read-only connector replaces where the figures come from, and
buying, selling and paying stay behind a human gate (`PLAN.md` § 7.4). This
runbook has no later version that ends in an order.

## What to do if the source is missing

If the folder is not there or holds no exports, return `status: blocked` and say
which folder you listed. Do not write a position from what the session said the
balance was: a number nobody exported is not a number, and money is the one
place where a confident guess is indistinguishable from a fact.

A read you were refused, by the person or by the round limit that ends a turn,
leaves an account out of the position. Name it in `open_questions`, leave its
lines out rather than estimating them, and return `status: needs_you`. A
position missing an account is useful; a position with an invented one is not.
"#;

/// How long the money lasts (PLAN 7.3, Phase 19, pack 4).
pub const BUDGET_RUNWAY_SKILL: &str = "budget.runway";

/// `budget.runway`: a range, not a point, with annual commitments spread
/// monthly. Never picks its own input.
const BUDGET_RUNWAY_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write
---

# budget.runway

## When to use it

When somebody asks how long the money lasts. It reads a position file and the
standing commitments and answers in months, with the arithmetic shown.

It answers a question; it does not decide anything about it. What to cut, what
to sell and what to take on are decisions, and they are somebody's.

## Inputs required and tools it will call

- The position file, as a path — one `budget.position` wrote. If you were not
  given one, that is the end of the run. A runway computed from whatever was
  written most recently is a runway for the wrong month, and it will read
  perfectly.
- The standing commitments: `.aegis/decisions/DECISIONS.md`, a contracts file, a
  subscriptions list — whatever this workspace keeps them in. And expected
  income, if any is written down anywhere.

Calls `fs_read` for those and `fs_write` for the note. It runs nothing, reaches
no account, and moves no money.

## Steps

1. `fs_read` the position file, and read its first line: that is how current
   this answer can be. A runway computed on a three-month-old position is a
   three-month-old runway, and it says so in its own first line.
2. List what goes out, one line each, with **how often** beside it and the file
   that says so. Monthly, quarterly, annual, one-off. Never a rate you inferred
   from a single charge: one appearance of a bill is one appearance, and an
   annual subscription billed in March is invisible for eleven months.
3. Put everything on the same period before adding anything — an annual figure
   divided by twelve, and the division shown. This is the step where a runway
   goes wrong, and it goes wrong quietly.
4. Do the same for what comes in, and count only what a file supports. Work that
   is likely, an invoice that will probably be paid, a client who usually
   renews: none of those is income, and each belongs in a line at the end saying
   what would change the answer.
5. Divide, and show the division: what is held, over what goes out net each
   month, is how many months. Write the numbers out so a reader can redo it.
6. Give a **range**, not a point. The low end assumes nothing uncertain arrives;
   the high end assumes all of it does. Say which assumption each end rests on.
   A single number is what somebody plans against, and the inputs almost never
   support one.
7. `fs_write` `.aegis/artefacts/runway-<date>.md`: how current the position is,
   what goes out, what comes in, the division, the range, and what would narrow
   it. End with the three things that would change the answer most.
8. Stop. Do not recommend a cut, do not propose a sale, and do not rank the
   outgoings by what you would drop first. That is the decision this note exists
   to inform.

## How to validate

Every outgoing names its file and its frequency. Every period conversion shows
its division. Nothing counted as income lacks a file behind it. The answer is a
range, and each end names the assumption it rests on. The note's first line says
how current the position under it is.

## What to return

`skill_return` with `status: done`, the note in `artefacts`, the position file
and the commitment files in `evidence`, and a summary of at most five lines: the
range in months, as of when, and what would narrow it.

`status: needs_you` when the position is older than the period you are dividing
by — a runway from a position older than a month is arithmetic on something that
has already changed — and when a commitment is named nowhere in writing. Say
which in `open_questions`.

## What requires approval

One `fs_write` inside the workspace. Nothing here spends, cancels, sells or
transfers, and no later version of it does: money leaving is behind a human gate
(`PLAN.md` § 7.4), and there is no tool in this build that could.

## What to do if the source is missing

No position file, no run: return `status: blocked` and ask for the path. Do not
build one on the way — that is `budget.position`, it has its own reconciliation,
and a runway resting on figures nobody reconciled is a confident number with
nothing under it.

If the commitments are not written down anywhere, say so and answer only from
what is: a workspace that has not recorded what it pays every month is a fact
worth the first line of the note. A read you were refused, by the person or by
the round limit that ends a turn, is an outgoing you did not count — name it,
leave it out, and return `status: needs_you`, because the runway you would have
written is too long rather than too short.
"#;

/// A line was crossed (PLAN 7.3, Phase 19, pack 4).
pub const BUDGET_ALERT_SKILL: &str = "budget.alert";

/// `budget.alert`: reports a threshold somebody else set being crossed, and
/// stops before any order or recommendation to trade (PLAN 7.3, 7.4).
const BUDGET_ALERT_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# budget.alert

## When to use it

When a figure has crossed a line somebody set. It says what moved, against what
threshold and since when. It proposes nothing.

One run covers one threshold. The threshold is one somebody wrote down before
the move — not one you draw now around what happened.

## Inputs required and tools it will call

- The threshold, and where it is written: a line in `.aegis/decisions/DECISIONS.md`,
  a limits file, a note from the human. If no file names it, this runbook does
  not apply, and saying so is the run.
- The figure, from a position file or the export it came from — the same figure
  the threshold was written about, not a related one.

Calls `fs_list` and `fs_read` for those and `fs_write` for the note. It runs
nothing, reaches no account, and places no order.

## Steps

1. `fs_read` the threshold where it is written, and quote it, with the date it
   was written. A threshold you cannot quote is a threshold nobody set.
2. `fs_read` the figure and its as-of date. Check it is the figure the threshold
   names — the same account, the same holding, the same currency. A threshold on
   one thing compared against another is the most convincing wrong alert there
   is.
3. Say by how much, and since when: the value now, the value at the threshold,
   the difference, and the last date the figure was on the other side of it.
   Show the subtraction.
4. Say what it is **not**. A number that crossed a line is not a cause: a
   currency move, a fee, a transfer between two accounts you are watching
   separately, a statement that arrived late. Name the ones you could rule out
   from the files and the ones you could not.
5. If crossing the line is what the file said would happen when nothing was
   wrong — a quarterly bill, an annual renewal, a known drawdown — say so in the
   first line. Most crossings are that.
6. `fs_write` `.aegis/artefacts/alert-<figure>-<date>.md`: the quoted threshold
   and its date, the figure and its as-of date, the difference with its
   arithmetic, what it is not, and the one question a person would need answered
   to decide. One question, not a list.
7. Stop. **No proposal.** Not *consider reducing*, not *it may be worth
   reviewing*, not an ordering of options. Aegis has no tool that buys, sells,
   transfers or cancels, this runbook has no later version that ends in one, and
   an alert that ends in a recommendation is a trade being placed one sentence
   at a time.

## How to validate

The threshold in the note is a quotation with the date it was written. The
figure carries its as-of date and is the one the threshold names. The
subtraction is shown. There is no sentence in the note recommending an action,
and there is exactly one question at the end of it.

## What to return

`skill_return` with `status: done`, the note in `artefacts`, the threshold file
and the figure's source in `evidence`, and a summary of at most five lines: what
crossed what, by how much, since when, and the one question.

`status: needs_you` when acting on it would be irreversible and time matters —
which is most of why a threshold was set — with the question in `open_questions`
and still no recommendation attached to it. The human decides and the human
acts; this run has done its whole job by being read in time.

`status: blocked` when no file names the threshold. Do not infer one from the
history of the figure. A line drawn around what already happened turns every
move into a crossing, and a watch that alerts on everything is a watch nobody
reads.

## What requires approval

One `fs_write` inside the workspace. Nothing else, and nothing else is possible:
there is no tool in this build that trades, pays or transfers, and a read-only
connector installed later replaces where the figure comes from rather than what
may be done with it (`PLAN.md` § 7.6). Money moving is a human act, every time
(§ 7.4).

## What to do if the source is missing

If the figure's source is not there, return `status: blocked` and name the path.
Do not alert on a number from the conversation: an alert is the artefact people
act on fastest and check least, which is exactly why it may not rest on
something nobody can go and read.

If you could read the threshold but not the figure, say so and return
`status: needs_you` — a threshold with no current figure beside it is a reason
to go and look, and that is a sentence worth writing. The other way round is
`blocked`.
"#;

/// Which of it, if any, is worth answering (PLAN 7.3, Phase 19, pack 5).
pub const SOCIAL_SCAN_SKILL: &str = "social.scan";

/// `social.scan`: the few posts worth answering against a criteria file, with
/// *none worth answering* as an ordinary result. "Someone is wrong" is never a
/// criterion.
const SOCIAL_SCAN_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# social.scan

## When to use it

When an export of mentions or a timeline is on disk and somebody has to decide
which of it, if any, is worth answering. Most of it is not.

The export is a file you put there. Nothing here connects to an account, reads a
live feed, follows anybody, or posts.

## Inputs required and tools it will call

- The export, as a path — `.aegis/briefs/` unless you were given another.
- The criterion: what this house answers, written down. A line in
  `.aegis/decisions/DECISIONS.md`, a note from the human, a file of standing
  policy. If no file says what is worth answering, this runbook does not apply,
  and saying so is the run.

Calls `fs_list` and `fs_read` for those and `fs_write` for the list. It runs
nothing, reaches no account, and answers nobody.

## Steps

1. `fs_read` the criterion first, before the export, and quote it in the file
   you write. Reading the posts first and deciding afterwards is how the
   criterion becomes whatever the loudest post was about.
2. `fs_read` the export. Take each item once: a quoted or reposted item is the
   same item, and a thread is one item, not one per message.
3. Keep exactly two reasons to answer, and require the post to meet one of them:
   somebody asked a question this house can answer **from a file**, or somebody
   is relying on something of ours that is wrong in a way we can correct with a
   fact. Nothing else qualifies.
4. **Being wrong is not a reason.** Not a bad take, not a misreading of the
   field, not a claim you could refute. A post nobody addressed to us, that
   nothing of ours depends on, is not an item however answerable it looks — and
   it will look very answerable, because that is what the material is written
   for.
5. Cap the list at three. If more than three qualify, keep the three where an
   answer would be most useful to the person who wrote them, and say how many
   you dropped. A list of eleven is a list nobody works through.
6. For each item: the handle, the date, the quoted sentence that qualifies it,
   which of the two reasons it meets, and the file that would answer it. An item
   with no file behind the answer is not on the list — that is a question for
   the human, not a draft waiting to happen.
7. `fs_write` `.aegis/artefacts/social-scan-<date>.md`: the quoted criterion, the
   items, how many were considered, and how many were dropped. Quote one sentence
   per item, not the post — other people's writing does not need to be copied
   into a repository to be found by the link beside it.
8. Stop. Do not draft anything. That is `social.reply`, one item at a time, and
   somebody chooses which.

## How to validate

The criterion in the file is a quotation from a file, not a sentence written
during this run. Every item names one of the two reasons and the file that would
answer it. No item is there because the post is wrong. The list is at most three
and says how many were considered.

## What to return

`skill_return` with `status: done`, the list in `artefacts`, the export path in
`evidence`, and a summary of at most five lines: how many items were considered
and which few qualified.

**None qualifying is the ordinary answer**, and a complete one: `status: done`,
no artefact, one line saying how many were read and that none met the criterion.
A scan that finds something worth answering every time is a scan manufacturing
obligation, and a queue of drafts nobody asked for is how a person ends up
posting more than they meant to.

`status: needs_you`, and no item written, when a post is about this house and
hostile — an accusation, a pile-on, somebody angry. That is not a draft; it is a
person's decision about whether to answer at all, and it is the one place in this
pack where speed makes things worse.

## What requires approval

One `fs_write` inside the workspace. Nothing here posts, replies, follows,
likes, reports or blocks, and there is no tool in this build that could. A
connector installed later replaces where the export comes from, not what may be
done with it (`PLAN.md` § 7.6).

## What to do if the source is missing

No export, or no file naming the criterion: `status: blocked`, saying which. Do
not scan from what the session has heard about; do not infer the criterion from
the posts. A criterion derived from what is in front of you selects the loudest
thing in front of you.

A read you were refused, by the person or by the round limit that ends a turn,
means the scan covered part of the export. Say which part, and return
`status: needs_you` only if what you could not read is where a question about
this house would have been.
"#;

/// Draft the answer to one post (PLAN 7.3, Phase 19, pack 5).
pub const SOCIAL_REPLY_SKILL: &str = "social.reply";

/// `social.reply`: one public, permanent answer. The steps name the shapes to
/// refuse (gratuitous corrections, openings about the other person being wrong)
/// and end by handing off to [`REVIEW_SKILL`].
const SOCIAL_REPLY_SEED: &str = r#"---
version: 1
tools: fs_read, fs_write
---

# social.reply

## When to use it

When one post deserves an answer and a person will publish it. One run drafts
one reply to one post.

It does not publish, and there is no later version of it that does.

## Inputs required and tools it will call

- The item, as a path or as the line from a `social.scan` list. If you were not
  given one, that is the end of the run: a reply drafted to whichever post was
  most interesting is a reply to the wrong person, and it will read well.
- The file that answers it — the one the scan named. And a handful of this
  house's own previous posts, if any are on disk, to write in the voice that is
  already there rather than one invented today.

Calls `fs_read` for those and `fs_write` for the draft. It lists nothing, runs
nothing, reaches no account, and publishes nothing.

## Steps

1. `fs_read` the item and the file that answers it. Answer the question that was
   asked. Not the question behind it, not the better question, and not the four
   other things in the post you could have said something about.
2. Give the fact and stop. If the fact makes the other person's claim wrong, the
   fact is enough — a sentence explaining that they were wrong is a sentence
   about them rather than about the thing, and it is the sentence that gets
   quoted on its own.
3. Refuse these shapes, whatever the post did: opening with *actually*; the
   correction that is not needed to answer; the joke at somebody's expense; the
   rhetorical question; the reply that is really an announcement. Each of them
   performs better than the plain answer, which is exactly the problem.
4. Every claim carries a file behind it, and a claim with no file does not get
   softer wording — it comes out of the draft. "Should be fixed soon", "we're
   looking at it", "probably next release" are commitments published to
   everybody, and they will be quoted back with a date attached.
5. Write it short: one or two sentences, in the language the post was written
   in. An answer that needs three paragraphs is not a reply — it is either a
   post of its own or a message to one person, and the runbook for a message to
   one person is `reply.draft`.
6. Then read it as a stranger with none of the context, and read it again with
   the post it answers cropped off. If either reading is worse than the plain
   truth, rewrite it. A quotable sentence you did not mean to write is the
   characteristic failure of this artefact.
7. `fs_write` `.aegis/artefacts/social-reply-<handle>-<date>.md`: the item and
   its link or path, the quoted question, the draft, and a **sources** block —
   one line per claim, naming the file it came from.
8. Stop. Publishing is the human's, after `never-send-without-review`, which is
   what that runbook was seeded for. Do not publish, do not schedule, and do not
   tell anybody an answer is coming.

## How to validate

The draft answers the quoted question in its first sentence. Every claim in it
has a line in the sources block. There is no sentence about the other person,
only about the thing. It survives being read with the question cropped off. It
is short enough to be read whole without expanding it.

## What to return

`skill_return` with `status: done`, the draft in `artefacts`, the item and the
source files in `evidence`, and a summary of at most five lines: what was asked,
what the reply says, and what it commits to.

`status: needs_you` when the honest answer is one this house has not decided —
whether something will ship, whether a bug is a bug, what something will cost —
with it in `open_questions` and the draft left unwritten. A public guess is a
commitment with an audience.

And `status: needs_you`, always, when answering would mean disagreeing with a
named person in public. That is a choice about how this house wants to be seen,
it is not reversible by deleting the post, and it is not a choice a runbook
makes on somebody's behalf.

## What requires approval

One `fs_write` inside the workspace. There is no tool here that publishes, and a
connector installed later does not change it: publish is on the same line as
send, pay, merge and deploy (`PLAN.md` § 7.4), so it stays a human act. A reply
is a file until a person posts it.

## What to do if the source is missing

No item, no run: `status: blocked`, and ask which one. Do not pick from the scan
list yourself — which post this house answers is a decision, and it is made by
the person who has to live with the answer.

If the file that would answer it is not there, say so and write nothing: an
answer with no source is the one kind of reply that cannot be taken back and
cannot be defended. Return `status: needs_you` with what you would have needed.
"#;

/// Say the thing that happened (PLAN 7.3, Phase 19, pack 5).
pub const SOCIAL_POST_SKILL: &str = "social.post";

/// `social.post`: every claim names something already done and on disk, each
/// sentence must survive being quoted alone, and nothing promises the future.
const SOCIAL_POST_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# social.post

## When to use it

When something has happened here that is worth saying in public — a release, a
write-up, a result. One run drafts one post from the files behind it.

It does not publish. Nothing in this build can.

## Inputs required and tools it will call

- What happened, and where it is on disk: a changelog entry, a tag, a merged
  change, a file that exists now and did not before. The post is written from
  that, not from a description of it.
- This house's previous posts, if any are on disk, so the draft sounds like
  whoever writes here rather than like a launch.

Calls `fs_list` and `fs_read` for those, and `fs_write` for the draft. It runs
nothing, reaches no account, and publishes nothing.

## Steps

1. `fs_read` what happened, in the files. If the thing being announced cannot be
   pointed at — a version that is not tagged, a feature not merged, a page not
   published — there is no post to write yet, and that is the answer.
2. Say what it is, in the first sentence, to somebody who has never heard of
   this project. No hook, no thread marker, no question the post then answers,
   no "we've been quiet lately". Those shapes are there to buy attention and
   they are the first thing that ages badly.
3. Only the past tense about this house. What shipped, what changed, what was
   measured. **No dates for anything that has not happened** — "next week",
   "soon", "in the coming months" are commitments published to everybody, and
   nobody will remember they were an aside.
4. Every number, name and claim comes from a file, and the file goes in the
   sources block. A benchmark needs its method beside it or it does not go in.
   This house is subject to the same rule `watch.sweep` applies to everybody
   else's announcements, and it is the same rule.
5. Do not compare with somebody else's product by name. A comparison is a claim
   about a thing you did not measure and cannot correct, made to an audience
   that includes them.
6. Read every sentence once, alone, as though it were the only one quoted. Then
   read the whole thing as somebody who dislikes this project. Rewrite anything
   that is worse under either reading — not to soften it, but because a sentence
   that only works in context will be read out of it.
7. `fs_write` `.aegis/artefacts/social-post-<date>-<subject>.md`: the draft, then
   a **sources** block naming the file behind every claim, then one line saying
   what in the draft is not yet true anywhere. That last line should be empty.
8. Stop. Publishing is the human's, after `never-send-without-review`. Do not
   publish, do not schedule it, do not write the follow-up, and do not draft the
   replies to it.

## How to validate

Every claim has a file in the sources block. Nothing in the post is in the future
tense about this house. No competitor is named. The first sentence makes sense to
somebody with no context. The line about what is not yet true is empty.

## What to return

`skill_return` with `status: done`, the draft in `artefacts`, the files behind it
in `evidence`, and a summary of at most five lines: what it announces and what it
claims.

`status: blocked` when the thing is not on disk yet. A post about something that
is about to be true is the most expensive artefact this pack can produce, because
it is the one that cannot be corrected and the one people screenshot. Say what
would have to exist, and stop.

`status: needs_you` when the post would be the first public word on something —
a price, a licence, a partnership, a person leaving. Those are announcements
before they are posts, and what they say is a decision somebody takes rather than
a draft somebody edits.

## What requires approval

One `fs_write` inside the workspace. Publish is on PLAN 7.4's line with send,
pay, merge, deploy and trade, so it stays a human act however the draft reads;
Aegis has no tool that posts, and a connector installed later would be one more
call put to a person, every time.

## What to do if the source is missing

If what you were asked to announce is not on disk, return `status: blocked` and
name what you looked for. Do not write it from the session's account of what
shipped — the session is where "it's basically done" lives, and this is the one
artefact where that sentence becomes public.

A read you were refused, by the person or by the round limit that ends a turn,
is a claim with no source: leave it out of the draft rather than out of the
sources block, and say so in the summary.
"#;

/// What somebody wants, written where it can be seen (PLAN 7.3, Phase 19,
/// pack 6).
pub const WISH_LIST_SKILL: &str = "wish.list";

/// `wish.list`: somebody's goals as a file, never a memory (PLAN 7.4). Nothing
/// acquires the grammar of a fact: unstated order is *unordered*, an unchecked
/// price is *not priced*. No judgement of the wants.
const WISH_LIST_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# wish.list

## When to use it

When somebody has said what they want and it should be written where it can be
seen. One run updates the goals file and judges nothing on it.

The goals are theirs: a car, a holiday, paying for a model subscription, a
machine, time off. What they are for is not this runbook's business.

## Inputs required and tools it will call

- The goals file, if there is one — `.aegis/artefacts/goals.md` unless you were
  given another path. `fs_list` first: an existing list is edited, never
  rewritten from what the conversation remembers of it.
- What was said this time, as the person said it.

Calls `fs_list`, `fs_read` and `fs_write`. It runs nothing, buys nothing, and
prices nothing by going and looking.

## Steps

1. `fs_read` the existing list before writing a word. What is already on it
   stays on it, in the words it is written in, unless the person said to change
   that entry.
2. Add only what was actually said. Not the thing that would obviously go with
   it, not the cheaper version, not the prerequisite you can see. A list nobody
   recognises as their own is a list they stop reading.
3. Take the ordering from the person, and record it as theirs. If they have not
   said what comes first, write *unordered* at the top and leave the entries in
   the order they arrived. An ordering invented here would be read next month as
   one they chose.
4. Price each entry only from something you were given or can read: a quote, an
   invoice, a page they saved, a figure they said. Anything else is **not
   priced**, in those words. Never an estimate — an estimate is what the funding
   pipeline will divide by, and by then nobody remembers it was made up.
5. Keep the date only if there was one. "Before the summer" is a date somebody
   said and goes in as that phrase; a month and a year you resolved it to is
   not.
6. Say what each entry is waiting on, in one line, when the person said: money,
   a decision, somebody else, nothing.
7. `fs_write` the list: the ordering and whose it is, then one entry per goal —
   what it is, what it costs or *not priced*, the date phrase or none, and what
   it waits on. Keep entries somebody has met, marked as met and dated, at the
   bottom; a list that only ever grows is a list of failures.
8. Stop. Do not propose how to pay for any of it, do not rank by what is
   achievable, and do not suggest dropping anything. How this gets funded is
   `revenue.pipeline`, and what to give up is nobody's call here.

## How to validate

Every entry is something the person said, in words they would recognise. The
ordering is attributed to them or the list says *unordered*. Every price names
where it came from, and everything else says *not priced*. No entry carries a
date nobody uttered. Nothing in the file evaluates whether a goal is a good
idea.

## What to return

`skill_return` with `status: done`, the list in `artefacts`, and a summary of at
most five lines: what was added or changed, and what is still not priced.

`status: needs_you` when two entries conflict — the same money twice, two dates
that cannot both hold — with both quoted and no resolution proposed. Which one
gives way is the whole of what a wish list is for deciding, and it is not a
tie-break a runbook performs.

There is no `blocked` for an empty list. A first run with no file writes the
first version, and a person with one goal has a goals file.

## What requires approval

One `fs_write` inside the workspace. Nothing here spends, orders, subscribes or
books, and there is no tool in this build that could. A goal is a file the
person can open, edit and delete in their own folder — deliberately not
something an identity remembers, which nobody else could read and which would
vanish with the identity.

## What to do if the source is missing

If the path you were given is not there and you were told to update rather than
create, return `status: blocked` and say which path you looked at. Do not
reconstruct somebody's goals from the conversation: a list rebuilt from memory
quietly loses the entries nobody has mentioned lately, which are usually the
ones that mattered longest.

A read you were refused, by the person or by the round limit that ends a turn,
means you have part of the list. Write nothing and return `status: needs_you`:
a partial list written whole is a list with entries silently deleted.
"#;

/// One idea, written well enough to be wrong (PLAN 7.3, Phase 19, pack 6).
pub const REVENUE_THESIS_SKILL: &str = "revenue.thesis";

/// `revenue.thesis`: a falsifiable proposal with what would disprove it and what
/// being wrong costs — no size, allocation or expected return. It may not read
/// the position or the wish list (see [`REVENUE_PIPELINE_SEED`]).
const REVENUE_THESIS_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# revenue.thesis

## When to use it

When an idea for making money should be written down properly enough to be
wrong. One run writes one proposal, with what would show it false.

A trade idea, a thing to sell, a way to charge for something that is free. It is
an argument on disk. Nothing here executes anything.

## Inputs required and tools it will call

- The idea, in one sentence, from a person. This runbook does not generate ideas
  to fill a folder.
- Whatever is on disk that bears on it: a watch entry, a note, a page somebody
  saved, earlier theses in `.aegis/artefacts/`.

Calls `fs_list` and `fs_read` for those and `fs_write` for the thesis. It runs
nothing, reaches no account and places no order.

**It does not read the position file, and it does not read the wish list.** Both
are about this house rather than about the world, and a thesis written with
either open is a thesis sized by what there is to lose or aimed at what somebody
wants to buy.

## Steps

1. Write the claim in one sentence, in the present or past tense about the
   world: what is true that other people have not priced in, what somebody would
   pay for that nobody is charging for. If it takes a paragraph, it is more than
   one claim, and each gets its own file.
2. Say what would have to be true for it to work, as things somebody could go
   and check. Not "if adoption continues" — a number, a shipped version, a
   published price, a filing, a contract.
3. Write the **falsifier**: what would show this is wrong, and by when it would
   show it. A thesis with no falsifier is not an idea, it is a mood, and this is
   the step that decides whether the file is worth keeping.
4. Write what being wrong costs, in the units it would be paid in — money,
   months, a reputation with somebody named, an opportunity that closes.
5. Say who is on the other side of it and why they are there. Somebody is
   selling what you would buy, or not charging for what you would charge for,
   and the reason is usually not stupidity.
6. **No size, no allocation, no expected return, no entry or exit.** Not "a
   small position", not "worth a few percent", not "10x if it works". Those are
   the order, they are the human's, and a number attached to a thesis is the
   part people read.
7. Do not say what this would pay for. Not the car, not the runway, not the
   subscription. A thesis explained by what it would fund is an argument written
   backwards, and it is the failure this pack exists to keep apart.
8. `fs_write` `.aegis/artefacts/thesis-<date>-<subject>.md`: the claim, the
   conditions, the falsifier and its date, the cost of being wrong, who is on
   the other side, and the files you read. Then stop.

## How to validate

The claim is one sentence about the world. Every condition is checkable rather
than judged. There is a falsifier and it carries a date. There is no size, no
allocation, no expected return and no target. Nothing in the file names a goal
this would pay for.

## What to return

`skill_return` with `status: done`, the thesis in `artefacts`, what you read in
`evidence`, and a summary of at most five lines: the claim, the falsifier, and
what being wrong costs.

`status: needs_you` when writing the falsifier shows there is not one — when
nothing over any horizon would tell you the idea was wrong. Say so plainly. That
is the single most useful sentence this runbook can produce, and it is worth
more than the file it did not write.

`status: blocked` when nobody gave you an idea. Do not go and find one. A folder
of theses this ran up on its own is a folder that gets read as research.

## What requires approval

One `fs_write` inside the workspace. There is no tool here that trades, sells,
posts or transfers, and no later version of this runbook ends in one: execution
of money movement is always a person's (`PLAN.md` § 7.3), and trade sits on
§ 7.4's line with send, pay, merge, publish and deploy.

## What to do if the source is missing

If a file you were pointed at is not there, say so and write the thesis without
it, marking what is unsupported. An argument that admits its gaps is usable; one
that quietly fills them is the kind that survives right up until money moves.

**Never delete or rewrite a thesis that turned out wrong.** When a falsifier
fires, append the date and what happened to the file that predicted it. A folder
of theses whose losers were edited out is the most misleading artefact this
whole library could hold, and the ones that were wrong are the only reason to
keep any of them.
"#;

/// What is funded, what is not (PLAN 7.3, Phase 19, pack 6).
pub const REVENUE_PIPELINE_SKILL: &str = "revenue.pipeline";

/// `revenue.pipeline`: shows wants and proposals side by side and the gap between
/// them, never claiming a proposal closes it (PLAN 7.3). The two prefixes keep
/// this apart from [`REVENUE_THESIS_SEED`].
const REVENUE_PIPELINE_SEED: &str = r#"---
version: 1
tools: fs_list, fs_read, fs_write
---

# revenue.pipeline

## When to use it

When somebody needs to see what is funded, what is not, and what each goal is
waiting on. It reports the gap and never claims to close it.

One run covers the goals as they stand today. It is a picture, not a plan.

## Inputs required and tools it will call

- The goals file — the one `wish.list` keeps.
- What is actually there: a position file `budget.position` wrote, or the
  decisions ledger, or whatever this workspace records committed money in.

Calls `fs_list` and `fs_read` for those and `fs_write` for the pipeline. It runs
nothing, spends nothing, and moves nothing.

## Steps

1. `fs_read` the goals file, and keep its ordering exactly. Whose ordering it is
   goes at the top of yours. Re-ranking the goals by what looks achievable is
   the one edit that would make this file feel helpful and make it somebody
   else's list.
2. `fs_read` what is actually available, and take its as-of date. A pipeline is
   as current as the position under it, and it says so in its first line.
3. For each goal in order: what it costs or *not priced*, what of it is covered,
   and the gap. A goal that is not priced has no gap — it has a missing price,
   and that is the line it gets.
4. Say what each goal is waiting on, taking it from the goals file rather than
   deciding: money, a decision, somebody else, nothing. Where it is waiting on
   money and the money is there, say that the wait is over — that is the one
   fact in this file somebody may want today.
5. **A proposal is not income.** A thesis in `.aegis/artefacts/` has no expected
   value, no probability and no line in this file. If one is mentioned at all it
   is in a closing list of *what exists as proposals*, by file name only, with
   no number beside it and no goal attached to it.
6. Do not connect a proposal to a goal, ever — not as a suggestion, not as an
   observation, not as "this would cover the second entry". That sentence is the
   whole reason these are two runbooks, and it is how a plan for a holiday
   becomes an argument for a trade.
7. Write the total gap plainly, and where the nearest unfunded goal is
   concerned, write the **condition** rather than a plan: what would have to be
   true for it to be funded — this much more, by this date, from something that
   already exists. Not a route to it, and not a suggestion about what to give up.
8. `fs_write` `.aegis/artefacts/pipeline-<date>.md`: how current the money
   figures are, the goals in the person's order with cost, covered and gap, what
   each waits on, the total gap, the condition on the nearest one, and the
   proposals by file name. Stop.

## How to validate

The goals are in the order the goals file has them, attributed to whoever set
it. Every money figure carries its as-of date and the file it came from. No
proposal has a number beside it. No line in the file connects a proposal to a
goal. Nothing suggests dropping or reordering anything.

## What to return

`skill_return` with `status: done`, the pipeline in `artefacts`, the goals and
money files in `evidence`, and a summary of at most five lines: how many goals
are funded, the total gap, and what the nearest one is waiting on.

`status: needs_you` when the money figures are older than the goals — a pipeline
built on a stale position is a picture of a month that has ended — and when a
goal's date has passed with a gap still open. The second is not a failure to
report gently: a date somebody set and did not meet is exactly what they asked
this file to show them.

## What requires approval

One `fs_write` inside the workspace. Nothing here spends, transfers, buys or
commits, and no later version of it does. What money moves and when is a human
decision every time (`PLAN.md` § 7.4); this file exists so that decision is
taken by somebody looking at the numbers rather than at a feeling about them.

## What to do if the source is missing

No goals file: `status: blocked`, and the thing to run is `wish.list`. Do not
assemble one from the conversation on the way past.

If there is no position file and no ledger, write the pipeline from the goals
alone, say in the first line that nothing on disk says what is available, and
return `status: needs_you`. A gap computed against a balance nobody exported is
a number that would be acted on, and it is the one kind of wrong this file must
not be.
"#;

/// `inbox.triage`, the workspace example runbook (Phase 13): a brief file in,
/// status and artefact out.
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

    /// Writes `.aegis/skills/<name>/<file>` under a workspace root.
    fn propose(root: &Path, name: &str, file: &str, text: &str) {
        let dir = workspace_dir(root).join(name);
        fs::create_dir_all(&dir).expect("skill dir");
        fs::write(dir.join(file), text).expect("file");
    }

    /// PLAN 7.13, *What the catalog sees*: only `SKILL.md`. A proposal is
    /// listed where a person can apply it and nowhere a model could run it.
    #[test]
    fn a_proposal_is_listed_and_never_reaches_the_catalog() {
        let root = TempDir::new().expect("workspace");
        let empty = TempDir::new().expect("library");
        propose(root.path(), "inbox.triage", PROPOSAL_FILE, TRIAGE_SEED);

        assert!(catalog(empty.path(), Some(root.path())).is_empty());
        assert!(is_proposed(root.path(), "inbox.triage"));

        let listed = proposals(root.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "inbox.triage");
        assert_eq!(listed[0].state, ProposalState::Pending);
        assert_eq!(listed[0].version, "1");
        assert_eq!(listed[0].problem, None);
        assert!(
            listed[0].target.ends_with(SKILL_FILE),
            "{}",
            listed[0].target
        );
    }

    #[test]
    fn a_proposal_says_whether_it_was_applied_or_would_replace_a_runbook() {
        let root = TempDir::new().expect("workspace");
        propose(root.path(), "applied", PROPOSAL_FILE, TRIAGE_SEED);
        propose(root.path(), "applied", SKILL_FILE, TRIAGE_SEED);
        propose(root.path(), "occupied", PROPOSAL_FILE, TRIAGE_SEED);
        propose(root.path(), "occupied", SKILL_FILE, REVIEW_SEED);

        let states: Vec<(String, ProposalState)> = proposals(root.path())
            .into_iter()
            .map(|proposal| (proposal.name, proposal.state))
            .collect();
        assert_eq!(
            states,
            vec![
                ("applied".to_owned(), ProposalState::Applied),
                ("occupied".to_owned(), ProposalState::Occupied),
            ]
        );
    }

    #[test]
    fn a_proposal_that_will_not_parse_is_listed_with_the_reason() {
        let root = TempDir::new().expect("workspace");
        propose(root.path(), "half", PROPOSAL_FILE, "no front matter here");

        let listed = proposals(root.path());
        assert_eq!(listed.len(), 1);
        assert!(listed[0].problem.is_some());
    }

    /// An apply is recognised by what the write is — the proposal, copied — and
    /// every other write of a `SKILL.md` is the handwritten path, left alone.
    #[test]
    fn an_apply_is_a_copy_of_the_proposal_and_nothing_else_is_one() {
        let root = TempDir::new().expect("workspace");
        propose(root.path(), "inbox.triage", PROPOSAL_FILE, TRIAGE_SEED);
        let target = Path::new(".aegis/skills/inbox.triage/SKILL.md");

        assert_eq!(
            apply_of(root.path(), target, TRIAGE_SEED),
            Some(Ok("inbox.triage".to_owned()))
        );
        // Spelled the way a case-folding filesystem would still reach.
        assert_eq!(
            apply_of(
                root.path(),
                Path::new(".Aegis/Skills/inbox.triage/skill.md"),
                TRIAGE_SEED
            ),
            Some(Ok("inbox.triage".to_owned()))
        );

        // A handwritten runbook, and the proposal written somewhere else.
        assert_eq!(apply_of(root.path(), target, REVIEW_SEED), None);
        assert_eq!(
            apply_of(root.path(), Path::new("notes/SKILL.md"), TRIAGE_SEED),
            None
        );
        assert_eq!(
            apply_of(
                root.path(),
                Path::new(".aegis/skills/other/SKILL.md"),
                TRIAGE_SEED
            ),
            None
        );
    }

    #[test]
    fn an_apply_is_refused_over_a_runbook_or_from_a_broken_proposal() {
        let root = TempDir::new().expect("workspace");

        propose(root.path(), "broken", PROPOSAL_FILE, "no front matter here");
        let refused = apply_of(
            root.path(),
            Path::new(".aegis/skills/broken/SKILL.md"),
            "no front matter here",
        );
        let reason = refused.expect("an apply").expect_err("refused");
        assert!(reason.contains("never applied"), "{reason}");

        propose(root.path(), "handwritten", PROPOSAL_FILE, TRIAGE_SEED);
        propose(root.path(), "handwritten", SKILL_FILE, REVIEW_SEED);
        let refused = apply_of(
            root.path(),
            Path::new(".aegis/skills/handwritten/SKILL.md"),
            TRIAGE_SEED,
        );
        let reason = refused.expect("an apply").expect_err("refused");
        assert!(reason.contains("never replaces"), "{reason}");
        assert_eq!(
            fs::read_to_string(
                workspace_dir(root.path())
                    .join("handwritten")
                    .join(SKILL_FILE)
            )
            .expect("still there"),
            REVIEW_SEED
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

    /// Every runbook of PLAN 7.3's Phase 19, in the order the packs landed.
    const PACKS: [&str; 18] = [
        REVIEW_DIFF_SKILL,
        DEPLOY_SKILL,
        ALERT_SKILL,
        MAIL_SKILL,
        THREAD_SKILL,
        REPLY_SKILL,
        WATCH_SWEEP_SKILL,
        WATCH_DIGEST_SKILL,
        WATCH_IMPACT_SKILL,
        BUDGET_POSITION_SKILL,
        BUDGET_RUNWAY_SKILL,
        BUDGET_ALERT_SKILL,
        SOCIAL_SCAN_SKILL,
        SOCIAL_REPLY_SKILL,
        SOCIAL_POST_SKILL,
        WISH_LIST_SKILL,
        REVENUE_THESIS_SKILL,
        REVENUE_PIPELINE_SKILL,
    ];

    /// The parsed runbook a seeded name ships with.
    fn seeded(name: &str) -> doc::SkillDoc {
        let (_, text) = SEEDED
            .iter()
            .find(|(seeded, _)| *seeded == name)
            .unwrap_or_else(|| panic!("`{name}` is seeded"));
        doc::parse(text).unwrap_or_else(|err| panic!("`{name}`: {err}"))
    }

    /// A seeded pack runbook declares no connector tool, or it would fail
    /// closed on a fresh install.
    #[test]
    fn a_domain_pack_calls_only_tools_this_build_has() {
        for name in PACKS {
            let doc = seeded(name);

            assert!(!doc.tools.is_empty(), "`{name}` declares what it calls");
            for tool in &doc.tools {
                assert!(
                    crate::tools::spec(tool).is_some(),
                    "`{name}` declares `{tool}`, which needs something installed"
                );
            }
        }
    }

    /// Each pack runbook's *When to use it* opening fits
    /// [`doc::SUMMARY_MAX_CHARS`], so its catalog line is not cut off.
    #[test]
    fn a_domain_pack_says_when_to_run_it_without_the_line_being_cut_off() {
        for name in PACKS {
            let summary = seeded(name).summary;
            assert!(
                !summary.ends_with('…'),
                "`{name}` opens with {} characters and the catalog keeps {}: {summary}",
                summary.chars().count(),
                doc::SUMMARY_MAX_CHARS
            );
        }
    }

    /// Seeded packs reach no model until an identity is granted them.
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

    /// Intake (pack 2) declares no command: its identity reads text strangers
    /// wrote and must not need a shell.
    #[test]
    fn the_intake_pack_holds_no_tool_that_runs_a_command() {
        for name in [MAIL_SKILL, THREAD_SKILL, REPLY_SKILL] {
            assert!(
                !seeded(name)
                    .tools
                    .iter()
                    .any(|held| held == tool::SHELL_EXEC),
                "`{name}` declares `{}`; intake is granted to an identity that has none",
                tool::SHELL_EXEC
            );
        }
    }

    /// The watch pack (pack 3) passes the real routine door
    /// ([`schedule::check`](crate::schedule::check)) and declares no command: an
    /// unattended run must not have an outbound channel.
    #[test]
    fn the_watch_pack_can_be_put_on_a_clock() {
        use crate::policy::Grant;
        use crate::store::routines::{RoutineDraft, Schedule};

        let dir = TempDir::new().expect("temp dir");
        seed(dir.path());
        let catalog = catalog(dir.path(), None);

        let mut watcher = agent_with(&[WATCH_SWEEP_SKILL, WATCH_DIGEST_SKILL, WATCH_IMPACT_SKILL]);
        watcher.name = "Watcher".to_owned();
        // Exactly what the three declare: no shell, no screen.
        watcher.tools = vec![
            tool::FS_LIST.to_owned(),
            tool::FS_READ.to_owned(),
            tool::FS_WRITE.to_owned(),
            tool::SKILL_RUN.to_owned(),
            tool::SKILL_RETURN.to_owned(),
        ];

        let draft = |name: &str, grants: Vec<Grant>| RoutineDraft {
            name: "Morning watch".to_owned(),
            project_id: "p1".to_owned(),
            agent_id: watcher.id.clone(),
            skill: name.to_owned(),
            schedule: Schedule::DailyAt { hour: 7, minute: 0 },
            grants,
            runs_per_day: 4,
        };

        for name in [WATCH_SWEEP_SKILL, WATCH_DIGEST_SKILL, WATCH_IMPACT_SKILL] {
            let skill = find(&catalog, name).expect("seeded");
            // The only standing approval needed: workspace writes.
            crate::schedule::check(
                &draft(name, vec![Grant::FsWrite]),
                &watcher,
                Some(skill),
                true,
            )
            .unwrap_or_else(|err| panic!("`{name}` cannot be scheduled: {err}"));
        }

        // No clock may amend the constitution: the door refuses that grant.
        let impact = find(&catalog, WATCH_IMPACT_SKILL).expect("seeded");
        assert!(
            crate::schedule::check(
                &draft(WATCH_IMPACT_SKILL, vec![Grant::WorldAmend]),
                &watcher,
                Some(impact),
                true,
            )
            .is_err(),
            "an écart is not something a routine signs for"
        );
    }

    /// Budget (pack 4) declares only `fs_read`, `fs_list` and `fs_write`: no
    /// shell that could become a broker client (PLAN 7.3, *not a broker*).
    #[test]
    fn the_budget_pack_could_not_reach_a_broker_if_it_tried() {
        let allowed = [tool::FS_LIST, tool::FS_READ, tool::FS_WRITE];

        for name in [
            BUDGET_POSITION_SKILL,
            BUDGET_RUNWAY_SKILL,
            BUDGET_ALERT_SKILL,
        ] {
            for declared in seeded(name).tools {
                assert!(
                    allowed.contains(&declared.as_str()),
                    "`{name}` declares `{declared}`; surveillance reads files and writes one back"
                );
            }
        }
    }

    /// Every runbook whose output a person sends or publishes ends by naming
    /// [`REVIEW_SKILL`] (PLAN 7.6, *Verifier is a skill*).
    #[test]
    fn a_draft_somebody_else_sends_names_the_runbook_that_checks_it() {
        for name in [
            ALERT_SKILL,
            REPLY_SKILL,
            SOCIAL_REPLY_SKILL,
            SOCIAL_POST_SKILL,
        ] {
            assert!(
                seeded(name).body.contains(REVIEW_SKILL),
                "`{name}` ends in something a person sends and never names `{REVIEW_SKILL}`"
            );
        }
    }

    /// Social (pack 5) declares only file tools, so no later edit quietly adds a
    /// way to publish (PLAN 7.4).
    #[test]
    fn the_social_pack_holds_nothing_that_could_publish() {
        let allowed = [tool::FS_LIST, tool::FS_READ, tool::FS_WRITE];

        for name in [SOCIAL_SCAN_SKILL, SOCIAL_REPLY_SKILL, SOCIAL_POST_SKILL] {
            for declared in seeded(name).tools {
                assert!(
                    allowed.contains(&declared.as_str()),
                    "`{name}` declares `{declared}`; a draft is a file until a person posts it"
                );
            }
        }
    }

    /// Revenue and wish list (pack 6) keep goals in files, never `memory_write`
    /// (PLAN 7.4: memories are per-identity, capped and deleted with it), with
    /// the budget pack's file-only perimeter.
    #[test]
    fn a_goal_is_a_file_and_not_something_an_identity_remembers() {
        let allowed = [tool::FS_LIST, tool::FS_READ, tool::FS_WRITE];

        for name in [
            WISH_LIST_SKILL,
            REVENUE_THESIS_SKILL,
            REVENUE_PIPELINE_SKILL,
        ] {
            for declared in seeded(name).tools {
                assert!(
                    allowed.contains(&declared.as_str()),
                    "`{name}` declares `{declared}`; a goal is a file somebody can open and delete"
                );
            }
        }
    }

    /// The whole seeded catalog stays within the per-line bound times its size;
    /// [`the_catalog_block_carries_no_step_of_any_runbook`] bounds one line.
    ///
    /// [`the_catalog_block_carries_no_step_of_any_runbook`]: self#tests
    #[test]
    fn a_library_holding_every_pack_still_costs_a_bounded_block() {
        let dir = TempDir::new().expect("temp dir");
        seed(dir.path());
        let catalog = catalog(dir.path(), None);

        let mut everything = Agent::builtin();
        everything.id = "a1".to_owned();
        everything.builtin = false;
        everything.skills = SEEDED.iter().map(|(name, _)| (*name).to_owned()).collect();

        let held = granted(&catalog, &everything);
        assert_eq!(held.len(), SEEDED.len(), "an identity granted all of them");

        let block = prompt_block(&held).expect("a block");
        let bound = SEEDED.len() * (doc::SUMMARY_MAX_CHARS + NAME_MAX_CHARS + 64) + 512;
        assert!(
            block.len() <= bound,
            "the whole library costs {} characters of every request, over the {bound} its own \
             per-line cap allows",
            block.len()
        );

        // And the property the cap exists to protect: it is still a catalog.
        assert!(
            !block.contains("## Steps"),
            "a runbook's steps reached the system message:\n{block}"
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
        // Every request carries the catalog, so one line must stay bounded.
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

    /// A pre-manifest library from the earliest build (no `cos.loop`) still
    /// receives [`COS_SKILL`]: the migration checks the disk, not
    /// [`SEEDED_BEFORE`].
    #[test]
    fn a_library_from_before_the_manifest_gains_what_it_was_never_actually_offered() {
        let dir = TempDir::new().expect("temp dir");
        let library = dir.path().join(LIBRARY_DIR);

        // A library exactly as the earliest build left it: one runbook, no
        // manifest, and no `cos.loop` — which that build could not have written.
        write_skill(&library, REVIEW_SKILL, REVIEW_SEED);

        seed(&library);
        let mut names: Vec<String> = catalog(&library, None)
            .into_iter()
            .map(|skill| skill.name)
            .collect();
        names.sort();
        let mut expected: Vec<&str> = SEEDED.iter().map(|(name, _)| *name).collect();
        expected.sort_unstable();
        assert_eq!(names, expected, "including the `cos.loop` it never got");

        // From here the manifest is the record, and a deletion is the user's.
        fs::remove_dir_all(library.join(CHECK_SKILL)).expect("the user deletes it");
        seed(&library);
        assert!(
            !library.join(CHECK_SKILL).exists(),
            "once offered, a runbook is the user's to keep or delete"
        );
    }

    /// The accepted cost of the disk check: a runbook deleted before the
    /// manifest existed comes back once, then a second deletion sticks.
    #[test]
    fn a_deletion_from_before_the_manifest_comes_back_once_and_then_never_again() {
        let dir = TempDir::new().expect("temp dir");
        let library = dir.path().join(LIBRARY_DIR);

        // Both of the pre-manifest names shipped here, and the owner deleted one.
        write_skill(&library, COS_SKILL, COS_SEED);

        seed(&library);
        assert!(
            library.join(REVIEW_SKILL).is_dir(),
            "it cannot be told apart from one that was never written"
        );

        fs::remove_dir_all(library.join(REVIEW_SKILL)).expect("the user deletes it again");
        seed(&library);
        assert!(
            !library.join(REVIEW_SKILL).exists(),
            "the manifest now records it, so the second deletion is final"
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
