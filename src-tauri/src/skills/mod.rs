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
mod seeded;

pub use seeded::*;

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
    /// The folders its writes land in (PLAN 7.23): what signing it onto a
    /// routine offers before `fs_write` over the whole workspace.
    pub writes: Vec<String>,
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
        writes: Vec::new(),
        path: path.display().to_string(),
        shadows: false,
        problem: None,
    };

    match read(path) {
        Ok(doc) => {
            skill.version = doc.version;
            skill.summary = doc.summary;
            skill.tools = doc.tools;
            skill.writes = doc.writes;
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
/// The name (not the body) carries across turns, since a halt can still split
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

#[cfg(test)]
mod tests;
