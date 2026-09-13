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

/// A runbook somebody proposed and nobody has applied yet (PLAN 7.13).
///
/// Beside where the `SKILL.md` would go, in the same directory, so applying one
/// is a copy between two names a person can see side by side. The catalog never
/// reads this name: [`read_dir`] looks for [`SKILL_FILE`] and nothing else, which
/// is what makes a proposal unrunnable as a property of discovery rather than a
/// check somebody has to remember.
pub const PROPOSAL_FILE: &str = "PROPOSAL.md";

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
// Proposals (PLAN 7.13)
// ---------------------------------------------------------------------------

/// Where a proposal stands against the runbook it would become.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ProposalState {
    /// There is no `SKILL.md` beside it yet, so it can be applied.
    Pending,
    /// The `SKILL.md` beside it is this proposal, byte for byte.
    ///
    /// Left listed rather than hidden: `fs_write` cannot delete, so an applied
    /// proposal stays on disk until a person removes it, and a panel that
    /// pretended it had gone would be describing a folder that is not theirs.
    Applied,
    /// A different `SKILL.md` is already there, and applying never replaces
    /// one. That runbook was not proposed through this path, so this path does
    /// not touch it (PLAN 7.13, *Never*).
    Occupied,
}

/// One `PROPOSAL.md` in a workspace, as Settings lists it.
///
/// The fields a person decides on — what it is for, what it would call, whether
/// it parses — and never the body. A proposal's body reaches nobody's system
/// prompt, and nothing in the window needs it: the path is on the row.
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

/// Every proposal in this workspace, by name.
///
/// The workspace only. A `PROPOSAL.md` in the library is not a proposal this
/// slice knows about: the library is the outside-the-workspace row, asked every
/// time, and session-authored runbooks belong in the project (PLAN 7.13,
/// *Workspace only*).
///
/// Never fails, for [`catalog`]'s reason.
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
/// `relative` is the write's target relative to the workspace root; `content` is
/// what would be written. An apply is recognised by what it *is* — a write of
/// `.aegis/skills/<name>/SKILL.md` whose content is the `PROPOSAL.md` beside it,
/// byte for byte — rather than by a flag the model sets, so there is no way to
/// ask for the apply row without actually copying the proposal.
///
/// * `None`: not an apply. Any other write of a `SKILL.md` is the handwritten
///   path of PLAN 7.6, which this slice does not replace and does not touch.
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
    // Case-folded on the fixed segments, for the filesystems that fold them:
    // `.Aegis/Skills/x/skill.md` is the same file there, and an apply that could
    // be dodged by spelling would be a rule about spelling.
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

/// Whether this workspace holds a proposal of this name.
///
/// For the refusal a `skill_run` of one gets, which has a different fix from a
/// name that is simply nowhere: this one is a person's apply away.
pub fn is_proposed(root: &Path, name: &str) -> bool {
    is_name(name) && workspace_dir(root).join(name).join(PROPOSAL_FILE).is_file()
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
/// A library that predates the manifest is reconciled against its own disk. What
/// it was offered cannot be read back, only inferred, and the two ways of
/// getting the inference wrong do not cost the same: calling a name offered when
/// it never was loses that runbook **for good and in silence**, while calling it
/// unoffered costs one example reappearing once, in the open, on the single
/// start that writes the manifest. So a name in [`SEEDED_BEFORE`] counts as
/// offered when its directory is actually there, and after that start the
/// manifest governs and a deletion is permanent.
///
/// Best effort throughout. A library that could not be created costs the
/// examples and nothing else: [`catalog`] reads a missing directory as an empty
/// one.
///
/// Twenty-four runbooks now, in eight groups. Two are the halves of the mode as it
/// was: the standing rule that nothing irreversible goes out unreviewed, and
/// the loop a Chief of Staff runs. Four are the world's (PLAN 7.2) — draft one,
/// perceive a delta, verify against the oracle, check the constitution. Three
/// are the client-delivery pack (PLAN 7.3, Phase 19, pack 1) — review a range,
/// draft a deploy, triage an alert. Three are client intake (pack 2) — turn a
/// message into a ticket, recap a thread, draft a reply nobody has sent. Three
/// are the watch (pack 3) — sweep what arrived into entries, digest what is new
/// since the last digest, and say what one entry would mean here. Three are
/// budget and portfolio (pack 4) — say what is held, say how long it lasts, and
/// say what crossed a line somebody set. Three are social (pack 5) — find the
/// few posts worth answering, draft one answer, draft one post. Three are
/// revenue and the wish list (pack 6) — keep somebody's goals, write one
/// proposal well enough to be wrong, and show what is funded.
///
/// Those last six groups are what a **domain pack** is, and the reason they
/// are here rather than anywhere else in this tree. Phase 19's rule is *domain
/// packs as skills, not runtime*: a domain reaches the harness as three files in
/// a directory, and `agent/turn.rs`, the policy matrix and the tool registry do
/// not know that a client exists. The rest of a pack is not in this file: the
/// connectors it may want are installed by the operator (Phase 18), and the
/// specialist that runs it is an identity somebody made and granted these names
/// to.
///
/// Each pack has one property visible from here, and in each case it is the
/// property that decides how the pack is granted.
///
/// * **Delivery** declares `shell_exec` and every runbook stops one step short
///   of the act that cannot be taken back — a merge, a deploy, a sent reply —
///   because that step is the human's (PLAN 7.4), and a procedure that ended
///   with it would be a procedure that had taken it.
/// * **Intake** declares no command at all, so its specialist is an identity
///   that cannot run one, which is what you want of the identity whose inputs
///   were written by people outside the house.
/// * **Watch** declares no command either, for a different reason: it is the
///   pack meant to run *unattended*, and an unattended run is never asked
///   anything — what it may do beyond reading is what somebody signed onto the
///   routine, and everything else is refused (PLAN 7.6). It also has no
///   irreversible act to stop short of, so its discipline is the other one:
///   *nothing happened* has to be a cheap and complete answer, or a watch on a
///   clock manufactures news the way a triage with no *no ask* manufactures
///   work.
/// * **Budget** declares no command for a third reason, and this one is about
///   the tool rather than the input or the hour: § 7.3 says *not a broker*, and
///   a program on PATH is a calculator right up until it is `ccxt`. It is also
///   the first pack whose material is arithmetic, where a wrong answer is
///   formatted exactly like a right one — so a figure is copied from a line
///   somebody else wrote or shown as a sum a reader can redo, and a total that
///   does not reconcile is `needs_you` rather than a rounded line. Its stop is
///   delivery's again, one item further down PLAN 7.4's list: the order.
/// * **Social** is the only one whose artefact is addressed to nobody in
///   particular, and the difference is permanence rather than accuracy. A
///   mistaken mail is fixed by a second mail to the same person; a post is read
///   by people who have none of the context, kept by some of them, and reached
///   by no correction. Its adversary is new too: intake's was a forger, which is
///   at least outside the run, and this one is inside it — the sharp answer
///   performs best, and a model asked for *a good reply* cannot tell good from
///   rewarded. So its steps name the shapes to refuse rather than asking for
///   judgement.
/// * **Revenue and the wish list** is the last of the six, and the only one
///   whose material has not happened. Every other pack's discipline is a
///   variant of *name the file the claim came from*; a want has no export
///   behind it and a proposal is an argument about a future. So the rule
///   inverts — nothing may acquire the grammar of a fact: an ordering nobody
///   stated is *unordered*, a price nobody looked up is *not priced*, and a
///   thesis carries what would show it false or it is not written. It is also
///   the only pack under two prefixes, which is load-bearing rather than
///   untidy: `revenue.thesis` may not read the wish list and
///   `revenue.pipeline` may not give a proposal a number, because a thesis
///   explained by the holiday it would fund is motivated reasoning with a file
///   behind it.
///
/// One rule now appears in three packs, which is worth reading as a property of
/// domain packs rather than of those three domains: `mail.triage` needs *no
/// ask*, `watch.digest` needs *nothing new*, `social.scan` needs *none worth
/// answering*. A procedure pointed at a pile and asked what to do about it will
/// always find something, so the empty answer has to be ordinary, cheap and
/// complete — not a fallback nobody reaches.
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
        // A library from before the manifest existed: it was offered whichever
        // of [`SEEDED_BEFORE`] is on its disk, because which of them it saw
        // depends on the build that made it and no file records the answer.
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

/// What a library created before [`SEEDED_FILE`] existed **may** have been
/// offered — not what it was.
///
/// Two names across two builds, and that is the whole problem. The first wrote
/// [`REVIEW_SKILL`] alone; [`COS_SKILL`] joined it later, behind a `if
/// library.exists() { return; }` that skipped every library already on disk. So
/// a library made by the first build never saw `cos.loop` and never could:
/// reading this list as a record of what was written is what left one install
/// without half of the mode, with the manifest recording it as offered. Which of
/// the two a given library actually got is answerable only by looking, which is
/// what [`seed`] does.
const SEEDED_BEFORE: [&str; 2] = [REVIEW_SKILL, COS_SKILL];

/// Every runbook this build seeds, and the body each starts as.
const SEEDED: [(&str, &str); 24] = [
    (REVIEW_SKILL, REVIEW_SEED),
    (COS_SKILL, COS_SEED),
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

// ---------------------------------------------------------------------------
// The client-intake pack (PLAN 7.3, Phase 19, pack 2)
// ---------------------------------------------------------------------------

/// Turn one inbound message into a ticket (PLAN 7.3, Phase 19: *mail first —
/// read + draft, never send*).
pub const MAIL_SKILL: &str = "mail.triage";

/// `mail.triage`, the runbook the rest of the intake pack works from.
///
/// The pack's source is a file, exactly as the delivery pack's was: an exported
/// message, a forwarded thread in `.aegis/briefs/`, a note passed on by the
/// human. There is no mail tool in this build and this runbook does not want
/// one — a connector later replaces where the message comes from and leaves the
/// procedure alone (PLAN 7.6), which is the same seam that let `review.diff`
/// read a diff through `shell_exec` on the first day.
///
/// What makes it a runbook rather than "read this email" is step 2. Mail is
/// indirect: an ask arrives as *would you have a moment at some point*, and a
/// model asked to summarize it returns a commitment with a deadline nobody
/// typed. Requiring the ask to be a **quoted sentence carrying its message's
/// date** makes *no ask* an available answer, which it has to be, because most
/// mail is no ask and a triage that finds work in every message is a triage
/// that manufactures it.
///
/// The `needs_you` in *What to return* is the one rule here that is not about
/// economy. A message asking for money to move, or for access, is the message
/// worth forging, it reads like the ordinary ones, and a ticket is not what
/// decides. Intake is the one pack whose inputs are written by strangers.
///
/// Step 1 came out of an exported message rather than out of writing this. An
/// `.eml` is mostly not text: a 180 KB attachment makes a 250 KB file, which is
/// under [`READ_MAX_BYTES`](crate::tools::READ_MAX_BYTES) and therefore arrives
/// *whole*, spending the turn on base64 — and one a little larger goes over the
/// cap, so the read comes back cut off inside the attachment and the message's
/// own last lines are never seen. Neither failure is visible from the runbook's
/// prose, which is why the step names the encoding header instead of saying
/// "read it".
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

/// `thread.recap`, the runbook that keeps a reply honest.
///
/// The one an intake pack is incomplete without, and the reason is arithmetic
/// rather than judgement: a mail client quotes the whole thread into every
/// message, so a nine-message thread carries one commitment forty times, and a
/// model reading it end to end finds a project where there was a sentence. Step
/// 2 is that; step 1 is the other half — the last message is not the state, and
/// reading backwards finds the version of a promise somebody restated rather
/// than the one they made.
///
/// The three lists are what [`REPLY_SEED`] draws its commitments from, and the
/// line between the first two is the whole value of the file: something is
/// *agreed* when one side proposed it and the other answered, and *outstanding*
/// otherwise. Silence reads as consent to anybody summarizing in good faith,
/// which is exactly how a client gets told that a thing they never agreed to
/// was settled weeks ago.
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

/// `reply.draft`, the intake pack's stop.
///
/// [`DEPLOY_SKILL`] is to a deploy what this is to a sent message, down to the
/// name: the procedure that can be written down is everything up to the
/// irreversible act, and the act stays a person's (PLAN 7.4). There is no mail
/// tool in this build, and the last step says so anyway — a connector arriving
/// later moves where a message comes from, not who sends one.
///
/// Its own rule is the sources block. A reply is the one artefact in this
/// application that ends up in somebody else's hands as a promise, so every
/// date, price and scope in it names the file it came from, and a commitment
/// with no file behind it does not get softer wording — it goes in
/// `open_questions`. "I'll look into it" is a commitment; the reader is right
/// to treat it as one.
///
/// It also refuses to pick its own input, which no other runbook here does. A
/// draft written to the newest ticket in the directory is how the wrong client
/// gets answered, and the failure is invisible because the reply is fluent.
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

/// `watch.sweep`, the collecting half of the watch pack.
///
/// There is no tool in this build that fetches anything, and this runbook does
/// not want one. Material reaches the watch the way a client's message reaches
/// intake: as files somebody put in the workspace. A connector later replaces
/// where the material comes from and leaves the procedure alone (PLAN 7.6) —
/// and installing one starts a program, which is the operator's act (Phase 18).
///
/// What makes it a runbook rather than "read these pages" is step 3. A watch
/// reads material written by people with something to sell, where the sentence
/// and the evidence for it are not the same object and only one of them is
/// usually present. Keeping *what it says* apart from *what it shows* is the
/// whole of an entry's value, because the digest above it can only be as honest
/// as the entries under it, and by then the launch post is gone.
///
/// Its bookkeeping is deliberately a set of file names rather than a ledger
/// file. What has been swept is answered by `fs_list` of the artefacts
/// directory, so nothing has to be kept in step with anything, and deleting an
/// entry is how you ask for that item to be read again.
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

/// `watch.digest`, the one runbook in this tree written to be put on a clock.
///
/// Every other seeded runbook answers something that happened: a client wrote,
/// a change is up for review, an alert fired. A watch runs whether or not
/// anything happened, which makes it the first pack whose cost is *recurring* —
/// and that changes what the failure is. Nothing here can be sent, so nothing
/// here needs a stop one step short of an irreversible act. What it needs
/// instead is for **nothing** to be a cheap and complete answer, because a
/// digest that always has five items is a digest manufacturing them, exactly as
/// a triage that finds an ask in every message manufactures work.
///
/// So an empty period writes no file and returns `done` rather than `blocked`:
/// a `blocked` for a quiet week would leave a routine two silences from pausing
/// itself over a watch working exactly as intended (PLAN 7.6, *Budgets*).
///
/// The delta is bookkept the way [`WATCH_SWEEP_SEED`]'s is, and for the same
/// reason: the digest ends with the entry names it covered, and the next run
/// reads that list first. It is what keeps a run proportional to what arrived
/// rather than to how long the watch has existed — the property a folder of two
/// hundred entries takes eight months to notice is missing.
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

/// `watch.impact`, where the watch meets this project and stops.
///
/// A watch is only worth running because something in it occasionally changes
/// what you do, and that is also the step where it goes wrong: *this exists* is
/// one sentence away from *we should switch*, and the sentence in between is the
/// one nobody writes. So the note carries the **condition** rather than the
/// conclusion — what would have to be true for this to be worth doing, in things
/// somebody could go and find out — and it costs doing nothing as well as doing
/// it, because a note that prices only the change is an argument for the change
/// wearing a table.
///
/// Its own stop is an écart (`COS.md` *Work*). This is the runbook most likely
/// of any in the library to conclude that the constitution would have to move,
/// which is the most useful thing it can conclude and the one thing it may not
/// act on. A specialist does not write `world/` — and a scheduled run is not
/// even offered that approval, since [`Grant::WorldAmend`] is refused at the
/// routine's door.
///
/// Like [`REPLY_SEED`] it refuses to pick its own input, for the same reason and
/// with the same failure: a note written about whatever looked most interesting
/// is a note about the wrong thing, and it will read well.
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

/// `budget.position`, the status file § 7.3 asks this pack for.
///
/// The first runbook in the library whose material is **arithmetic** rather
/// than prose, which is a different failure and a worse one. A model asked to
/// total a column returns a plausible number, formatted beautifully, and
/// nothing about the artefact looks wrong — a wrong review argues with you, a
/// wrong total does not.
///
/// So the rule is that a figure is either **copied** from a line somebody else
/// wrote or **shown** as an arithmetic a reader can redo, and there is no third
/// kind. That is also the answer to the obvious objection: this pack declares
/// no `shell_exec`, so nothing here runs a calculator. It does not need one. A
/// model that shows its addends and reconciles them against the statement's own
/// stated total is caught when it adds wrong; one that reports only the total
/// never is. Where the arithmetic should be done by a program, that program is
/// a connector the operator installs (Phase 18), and it replaces where the
/// number is computed rather than the procedure (PLAN 7.6).
///
/// The other half is staleness, which is the failure specific to money: a
/// figure with no date is not a figure, and a position is only as current as
/// its oldest input. So the file leads with the stalest as-of date among its
/// sources rather than with today's.
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

/// `budget.runway`, the question the status file exists to answer.
///
/// Two failures, and both are arithmetic wearing prose. The first is the
/// annualised commitment counted as a monthly one, or missed because it only
/// appears once in a year of exports — a subscription billed in March is
/// invisible in April and is a twelfth of itself every month. The second is the
/// point estimate: "eleven months" from inputs that support "nine to fourteen"
/// is a number somebody will plan against, and the honest artefact is the range
/// plus what would narrow it.
///
/// It also refuses to pick its own input, as [`REPLY_SEED`] and
/// [`WATCH_IMPACT_SEED`] do. A runway computed from whatever position file was
/// most recently written is a runway for the wrong month, and it will read
/// perfectly.
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

/// `budget.alert`, the surveillance half, and the pack's stop.
///
/// Delivery stops before the deploy and intake before the send; this stops
/// before the **order**, which is on the same list (PLAN 7.4) and is the one
/// this pack is most often one sentence away from. § 7.3 is blunt about it —
/// *read-only connectors, a status file, alerts. Not a broker* — and the reason
/// the sentence has to be in the runbook rather than only in the plan is that a
/// number crossing a line reads as an instruction. An alert that ends in
/// *consider reducing the position* is an alert that has traded, slowly.
///
/// Its own rule is that a threshold is somebody else's. A line the run picked
/// while writing the note is a line drawn around what happened, which is how a
/// watch on a portfolio ends up reporting every move as significant.
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

/// `social.scan`, and the third time this library has had to make *nothing* an
/// available answer.
///
/// `mail.triage` needed *no ask*, `watch.digest` needed *nothing new*, and this
/// needs *none worth answering* — which is worth saying out loud, because three
/// packs arriving at the same rule is not a coincidence about those domains. The
/// characteristic failure of a domain pack is manufacturing work: a procedure
/// pointed at a pile and asked what to do about it will always find something,
/// and the cheapest way to stop that is to make the empty answer explicit,
/// ordinary and complete rather than a fallback nobody reaches.
///
/// What is new here is the adversary. Intake's was a forger, and a forger is at
/// least outside the run. This one is inside it: the material was written to be
/// engaging, and the post that most invites an answer is the one somebody is
/// wrong on. *Being wrong* is never in the criterion, and the criterion has to
/// be a file rather than a judgement made while reading — the same shape as
/// [`BUDGET_ALERT_SEED`]'s threshold, for the same reason, and against a pull
/// that is much stronger here.
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

/// `social.reply`, written against the gradient rather than against a mistake.
///
/// Every other drafting runbook here can be got right by being careful. This one
/// has something pulling at it: of the answers that could be written to a post,
/// the sharp one performs best, and a model asked for *a good reply* has no way
/// to tell the difference between good and rewarded. So the steps name the
/// shapes to refuse rather than asking for judgement — no correction that is not
/// load-bearing, no reply whose first clause is about the other person being
/// wrong, no answer to the argument instead of the question.
///
/// The other half is that a reply is **public and permanent**, which the mail
/// pack's is not. `reply.draft` goes to a named person in a thread that carries
/// its own context, and a mistake in it is fixed by a second mail to the same
/// person. This goes to everybody, it will be read by people who have none of
/// the context, it can be quoted with the question cropped off, and no
/// correction reaches the people who read the first one. That is why publish
/// sits on PLAN 7.4's list beside sending and deploying, and why the last step
/// hands the draft to [`REVIEW_SKILL`].
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

/// `social.post`, the only artefact in this tree addressed to nobody in
/// particular.
///
/// Everything else the library writes has a reader: a client, a colleague, the
/// person who set a threshold, whoever opens the digest on Friday. A post has an
/// audience instead, most of whom will arrive without the context, some of whom
/// will keep a copy, and none of whom will see the correction. So the two rules
/// are about permanence rather than about accuracy: every claim names something
/// that has *already happened* and is on disk, and every sentence is read once
/// on its own, out of context, before the draft is written out.
///
/// The forward-looking sentence is the one this exists to stop. "Coming next
/// week" costs nothing to write and is a published deadline; it is also the
/// sentence a model reaches for, because a post about something finished feels
/// like it needs one.
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

/// `wish.list`, and the first artefact in this library with nothing behind it.
///
/// Every other runbook's discipline is a variant of one sentence: name the file
/// the claim came from. A diff, a message, a release note, a statement, a post —
/// all of them happened, and the rule is that the artefact may not go beyond
/// them. A want has not happened and may never. There is no export of somebody
/// wanting a car.
///
/// So the rule inverts. What the file must never do is let a want acquire the
/// grammar of a fact: the ordering is the one the person stated, recorded as
/// theirs, and where they stated none the list says *unordered* rather than
/// picking; a price nobody looked up is *not priced* rather than an estimate,
/// because an estimate is the number [`REVENUE_PIPELINE_SEED`] then divides by.
///
/// It is also the runbook that has no opinions. PLAN 7.3 is that the CoS keeps
/// the list visible and the human decides the spend, and AGENTS.md's line for
/// this workload is *not a shopping agent*. Nothing here judges whether a want
/// is sensible, drops one that looks unwise, or adds one nobody asked for.
///
/// And it is a **file**, which PLAN 7.4 says in as many words — funding goals
/// expressed as files. Not a memory on an identity: a memory is invisible,
/// capped, unreadable by anybody else and gone when the identity is deleted,
/// which is four things a person's own goals should never be.
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

/// `revenue.thesis`, which is a proposal and never an order.
///
/// PLAN 7.3 asks for *proposals* — a trade thesis, a monetization draft — and
/// PLAN 7.4 keeps trading and X monetization as funding goals expressed as
/// files, not as a reason to put a broker in `src-tauri`. What that leaves is an
/// argument, and an argument's only defence against being fluent is being
/// falsifiable. So every thesis carries what would show it false and what being
/// wrong costs, and it carries no size, no allocation and no expected return —
/// those three are the order wearing a thesis.
///
/// Its sharpest rule is that it may not read the position. A thesis is about the
/// world; the balance is about this house; and reading the second while writing
/// the first is exactly how a thesis gets sized by what is available to lose.
/// That is the same wall [`REVENUE_PIPELINE_SEED`] holds from the other side,
/// and it is why the wish list is not in this runbook's inputs either: *this
/// could pay for the car* is motivated reasoning with a file behind it, which is
/// worse than motivated reasoning without one.
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

/// `revenue.pipeline`, the one file allowed to hold both halves of this pack,
/// and the wall between them.
///
/// PLAN 7.3 gives the CoS this job in one clause — keep the wish list and the
/// funding pipeline visible, not click "buy" — and *visible* is not *joined*. A
/// wish list beside a folder of revenue proposals is one step from "here is how
/// to pay for the car", and the most expensive artefact this pack could produce
/// is a thesis whose real cause is a holiday.
///
/// So the pipeline reports the gap and never claims anything closes it: a
/// proposal has no expected value here and the file has no column for one.
/// [`REVENUE_THESIS_SEED`] holds the same wall from the other side by refusing
/// to read the wish list at all. Together they are the reason this pack is two
/// prefixes rather than one — the naming keeps apart what the arithmetic would
/// happily join.
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

    /// A domain pack (PLAN 7.3, Phase 19) calls nothing that has to be
    /// installed first.
    ///
    /// A pack is skills, connectors and an identity, and only the first of
    /// those ships. A seeded runbook that declared `coolify__deploy` or
    /// `imap__fetch` would be an example that fails closed at `skill_run` on
    /// every machine where that connector is not installed — which is every
    /// machine, on the day it is seeded. The source of a diff, a deploy fact or
    /// a client's message is `shell_exec` and files on disk today; a connector
    /// replaces the source later, and that is an edit to the runbook the
    /// operator owns.
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

    /// A pack's catalog line is the whole of what a model sees before choosing
    /// it.
    ///
    /// That line is the first paragraph of *When to use it*, capped at
    /// [`doc::SUMMARY_MAX_CHARS`] and ellipsised past it — so an opening
    /// paragraph that runs long does not cost more prompt, it costs the end of
    /// the sentence saying when to run the thing, which is the one sentence a
    /// catalog is for. Six domain runbooks are already more than a person keeps
    /// in their head, and whichever pack lands next, its first paragraph fits.
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

    /// Intake (PLAN 7.3, Phase 19, pack 2) holds no tool that runs a command.
    ///
    /// The property that decides how the pack is granted, which is why it is a
    /// test rather than a sentence in the README. Its three runbooks read files
    /// and write files, so the specialist that holds them needs `fs_*` and
    /// nothing else — and an identity with no `shell_exec` is what you want
    /// pointed at text somebody outside the house wrote. A runbook here that
    /// grew a `git` call would quietly turn that identity into one that has a
    /// shell, on a grant nobody revisited.
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

    /// The watch (PLAN 7.3, Phase 19, pack 3) can actually be put on a clock.
    ///
    /// The property that decides how *this* pack is granted, and the one the
    /// other two never had to have: § 7.3 gives watch as **scheduled** research,
    /// so a runbook here that cannot pass the routine door is a runbook that
    /// does not do the thing the pack is for. The door is the real one —
    /// [`schedule::check`](crate::schedule::check) — because every part of it is
    /// a claim about these files: the tools are declared under *Inputs required
    /// and tools it will call*, the identity holds them, and every standing
    /// approval a run needs is one a person may sign.
    ///
    /// Which is why the pack declares no command, for a different reason than
    /// intake's. A `curl` signed once and fired at four in the morning is an
    /// outbound channel with nobody on it. Fetching stays the operator's act
    /// — material arrives in the workspace, or a connector they installed puts
    /// it there — and the runbooks read files and write one file back.
    #[test]
    fn the_watch_pack_can_be_put_on_a_clock() {
        use crate::policy::Grant;
        use crate::store::routines::{RoutineDraft, Schedule};

        let dir = TempDir::new().expect("temp dir");
        seed(dir.path());
        let catalog = catalog(dir.path(), None);

        let mut watcher = agent_with(&[WATCH_SWEEP_SKILL, WATCH_DIGEST_SKILL, WATCH_IMPACT_SKILL]);
        watcher.name = "Watcher".to_owned();
        // Everything the three declare, and nothing else. `shell_exec` and
        // `screen_capture` are not on this identity, which is the point.
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
            // One standing approval: write inside the workspace. Everything
            // else a run of these wants is a read, and reads inside the
            // workspace are not asked about in the first place.
            crate::schedule::check(
                &draft(name, vec![Grant::FsWrite]),
                &watcher,
                Some(skill),
                true,
            )
            .unwrap_or_else(|err| panic!("`{name}` cannot be scheduled: {err}"));
        }

        // And the one conclusion `watch.impact` is likeliest to reach is the
        // one no clock may act on: amending the constitution is a human
        // decision (`COS.md` *Work*), so the door refuses the grant rather than
        // the runbook discovering it at four in the morning.
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

    /// Budget (PLAN 7.3, Phase 19, pack 4) could not reach a broker if it tried.
    ///
    /// § 7.3 gives this pack in five words — *read-only connectors, a status
    /// file, alerts. Not a broker* — and the last three are the ones that need
    /// enforcing, because they are the ones a convenient edit undoes. So the
    /// assertion is stronger than intake's: not "no `shell_exec`" but **these
    /// three tools and no others**. Reading files, listing them, writing one
    /// back is the whole perimeter of surveillance, and everything outside it is
    /// something this pack has no use for and a portfolio identity should not
    /// hold — a shell (a program on PATH is a calculator right up until it is a
    /// broker's client), a screen, another identity's attention, a connector
    /// call nobody watched.
    ///
    /// Which leaves the arithmetic to be done by the model, and that is the
    /// point rather than an oversight: the runbooks answer it by showing the
    /// addends and reconciling against the source's own stated total, so an
    /// error is *visible*. Where the sum should be computed by a program, that
    /// program is a connector the operator installs, replacing where the number
    /// comes from and not the procedure (PLAN 7.6).
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

    /// A draft somebody else sends names the runbook that checks it first.
    ///
    /// Not a rule invented for this pack — an invariant the library already had
    /// and nobody had written down. Of the twenty-one seeded runbooks, exactly
    /// the ones whose output is a message a person will send end by handing it
    /// to [`REVIEW_SKILL`], and that runbook was seeded first precisely to be
    /// the other end of this (PLAN 7.6, *Verifier is a skill*). A runbook naming
    /// another is the split the format is for; four of them naming this one is
    /// the standing rule of the whole mode having somewhere to attach.
    ///
    /// It matters most where it was added last. Social is the pack whose drafts
    /// go to nobody in particular and stay there — a mistaken mail is fixed by a
    /// second mail to the same person, and no correction reaches the people who
    /// read a post. Publish sits on PLAN 7.4's line with send, pay, merge and
    /// deploy for that reason, and a draft that reached the end of its runbook
    /// without naming the review is a draft one step from being published by
    /// momentum.
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

    /// Social (PLAN 7.3, Phase 19, pack 5) has no tool that could publish.
    ///
    /// The same set assertion the budget pack gets, and for the sharper half of
    /// the same reason: § 7.4 puts *publish* on one line with send, pay, merge,
    /// deploy and trade. Aegis has no tool that posts, so the perimeter is not
    /// enforcing an absence today — it is what makes the absence survive the
    /// edit where a runbook grows a `shell_exec` to "just check the API", on an
    /// identity somebody granted once and has not looked at since.
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

    /// A goal is a file, and not something an identity remembers.
    ///
    /// PLAN 7.4 says it in as many words — trading, X monetization and the wish
    /// list are *funding goals expressed as files* — and the tempting shortcut
    /// is the one this forbids: `memory_write` is right there, a want is exactly
    /// the shape of a thing to remember, and a runbook that recorded goals as
    /// memories would look tidier than one that keeps a markdown file.
    ///
    /// It would also be wrong in four ways at once, and all four are properties
    /// of memories rather than opinions about them: a memory belongs to one
    /// identity and no other can read it, there is no view spanning two, an
    /// identity holds at most two hundred, and deleting the identity forgets
    /// what it knew. Somebody's own goals must not be invisible, capped,
    /// unreadable by the next specialist, or destroyed by an edit in Settings.
    /// A file in their folder is none of those things.
    ///
    /// The rest of the perimeter is the budget pack's, for the reason PLAN 7.3
    /// gives this one: execution of money movement is always human.
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

    /// What a full library costs every turn, now that all six packs have landed.
    ///
    /// [`the_catalog_block_carries_no_step_of_any_runbook`] bounds what *one
    /// more* runbook costs. This bounds the whole of it, which is the question
    /// Phase 19 made worth asking: eighteen of the twenty-four seeded runbooks
    /// arrived as domain packs, one pack at a time, each diff boring enough that
    /// nobody was counting — and the catalog is in the system message of every
    /// request an identity granted them makes (PLAN 7.6, *catalog in, body on
    /// demand*). Six more packs added the same way, with nothing watching the
    /// total, is how a library becomes a context window.
    ///
    /// The bound is the per-line one multiplied out, so it stays true of the
    /// seventh pack without being edited, and it fails if a line ever stops
    /// being bounded — which is the thing that would actually go wrong.
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
    /// without every existing install being the one install that never sees it.
    ///
    /// Written against the install that *was* that one install. The earliest
    /// build seeded [`REVIEW_SKILL`] alone and created the library by writing
    /// it; [`COS_SKILL`] was added to the pair one phase later, behind a guard
    /// that skipped any library already on disk, so a library made by the first
    /// build never saw it. Reading [`SEEDED_BEFORE`] as a record of what was
    /// written then recorded `cos.loop` as offered in the new manifest, which is
    /// how an install ends up permanently missing half of the mode without a
    /// line anywhere saying so. The migration therefore looks at the disk.
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

    /// The one thing the disk check costs, paid once and in the open.
    ///
    /// A runbook deleted from a library that never got a manifest comes back on
    /// the start that writes one, because nothing on disk distinguishes "deleted
    /// it" from "never had it". That is the cheap side of the asymmetry: the
    /// user deletes it a second time and the manifest makes it stick, where the
    /// other guess loses a runbook silently and forever.
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
