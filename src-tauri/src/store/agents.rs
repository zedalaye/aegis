//! The agent document: `agents.json` (PLAN 7.3, Phase 12).
//!
//! An identity: name, role, instructions, provider, and two allow-lists (tools
//! and skills). Sessions bind to one at creation; this module is data only.
//!
//! * **The default is built in** ([`Agent::builtin`]): a constant, so it cannot
//!   be deleted or edited, and pre-Phase-12 sessions resolve to it.
//! * **Tools are registry names** ([`tools::registry`](crate::tools::registry)),
//!   stored in registry order.
//! * **Instructions are capped**: procedure belongs in a skill (PLAN 7.1, 7.6).
//! * **Skills are a second allow-list** that grants no tool, and require
//!   `skill_run` and `skill_return` in the tool list.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};
use crate::policy::tool;
use crate::skills;
use crate::store::connectors;
use crate::tools;

/// Name of the document under the application-data directory.
const AGENTS_FILE: &str = "agents.json";

/// Schema version of [`AgentsFile`], independent of the other documents.
const SCHEMA_VERSION: u32 = 1;

/// The identity a session runs as when it named none. Reserved: no stored
/// agent carries it.
pub const DEFAULT_AGENT_ID: &str = "default";

/// The only provider binding this build resolves: the one in Settings. Named
/// per identity so a provider roster can land later (PLAN 7.1).
pub const DEFAULT_PROVIDER_ID: &str = "default";

/// Longest identity name.
const NAME_MAX_CHARS: usize = 48;

/// Longest role line, shown beside the name in pickers.
const ROLE_MAX_CHARS: usize = 160;

/// Longest instruction block; procedure beyond it is a skill (PLAN 7.6).
const INSTRUCTIONS_MAX_CHARS: usize = 2000;

/// Most skills one identity may be granted.
const SKILLS_MAX: usize = 64;

/// Most scheduled runs one identity may make in a day, across all its routines
/// (`COS.md`). Interactive turns are not counted.
pub const AGENT_RUNS_PER_DAY_MAX: u32 = 200;

/// What a new identity's daily ceiling starts at.
pub const AGENT_RUNS_PER_DAY_DEFAULT: u32 = 48;

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 7.3, Phase 12)
// ---------------------------------------------------------------------------

/// An identity, as the UI and the runtime see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Agent {
    /// UUID v4, or [`DEFAULT_AGENT_ID`] for the built-in one.
    pub id: String,
    /// Display name: "Reviewer", "Scribe".
    pub name: String,
    /// What this identity is for, in one line.
    pub role: String,
    /// What it carries into every system message; empty for the built-in one.
    pub instructions: String,
    /// Which provider answers for it. [`DEFAULT_PROVIDER_ID`] today.
    pub provider_id: String,
    /// The tools it may call, in registry order: the only schemas shown, and
    /// policy refuses the rest.
    pub tools: Vec<String>,
    /// The skills it may run (Phase 13). Never widens [`Agent::tools`]; empty
    /// for the built-in identity.
    pub skills: Vec<String>,
    /// Most scheduled runs it may make in a day (Phase 16); zero means never
    /// scheduled.
    pub runs_per_day: u32,
    /// Whether this is the built-in identity, which cannot be edited or
    /// deleted. Derived, never stored.
    pub builtin: bool,
}

impl Agent {
    /// The identity a session resolves to when it named none: every registered
    /// tool, no instructions, no skills — the pre-Phase-12 assistant.
    pub fn builtin() -> Self {
        Self {
            id: DEFAULT_AGENT_ID.to_owned(),
            name: "Assistant".to_owned(),
            role: String::new(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            tools: tools::names()
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            skills: Vec::new(),
            // It holds no skills, so nothing can schedule it anyway (a routine
            // names a granted skill). The default is written out rather than
            // zeroed so that the number means the same thing on every row.
            runs_per_day: AGENT_RUNS_PER_DAY_DEFAULT,
            builtin: true,
        }
    }

    /// The identity for a session whose agent is missing (hand-edited file):
    /// no tools at all, so damage never widens privileges.
    pub fn stranded(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: "Unknown identity".to_owned(),
            role: "this session names an identity that is no longer on file".to_owned(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            tools: Vec::new(),
            skills: Vec::new(),
            runs_per_day: 0,
            builtin: false,
        }
    }

    /// Whether this identity may call `tool`.
    pub fn allows(&self, tool: &str) -> bool {
        self.tools.iter().any(|granted| granted == tool)
    }

    /// Whether this identity may run `skill`. The built-in identity runs none.
    pub fn allows_skill(&self, skill: &str) -> bool {
        self.skills.iter().any(|granted| granted == skill)
    }
}

/// What a create or an update carries.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct AgentDraft {
    /// Display name. Trimmed, and unique among identities.
    pub name: String,
    /// What this identity is for, in one line.
    pub role: String,
    /// What it carries into every request. May be empty.
    pub instructions: String,
    /// Which provider answers for it.
    pub provider_id: String,
    /// Tool names from the registry. May be empty — an identity that only reads
    /// and writes prose is a useful thing to be able to make.
    pub tools: Vec<String>,
    /// The runbooks it may run. Requires `skill_run` and `skill_return` in
    /// [`AgentDraft::tools`] when it is not empty.
    pub skills: Vec<String>,
    /// Most scheduled runs a day, capped at [`AGENT_RUNS_PER_DAY_MAX`];
    /// defaulted for older callers.
    #[serde(default = "default_runs_per_day")]
    pub runs_per_day: u32,
}

/// The ceiling a draft that names none carries.
const fn default_runs_per_day() -> u32 {
    AGENT_RUNS_PER_DAY_DEFAULT
}

// ---------------------------------------------------------------------------
// On-disk shapes
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentsFile {
    version: u32,
    agents: Vec<StoredAgent>,
}

/// An identity as persisted: not [`Agent`], so no row can claim `builtin`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAgent {
    id: String,
    name: String,
    role: String,
    instructions: String,
    provider_id: String,
    tools: Vec<String>,
    #[serde(default)]
    skills: Vec<String>,
    /// `#[serde(default)]` is the migration: an identity written before Phase
    /// 16 reads back with the same ceiling a new one gets, and nothing could
    /// have scheduled it before there were routines.
    #[serde(default = "default_runs_per_day")]
    runs_per_day: u32,
    created_at: String,
    updated_at: String,
}

impl StoredAgent {
    /// The identity as the UI sees it.
    fn to_agent(&self) -> Agent {
        Agent {
            id: self.id.clone(),
            name: self.name.clone(),
            role: self.role.clone(),
            instructions: self.instructions.clone(),
            provider_id: self.provider_id.clone(),
            tools: self.tools.clone(),
            skills: self.skills.clone(),
            runs_per_day: self.runs_per_day,
            builtin: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The agent store: stored identities plus the built-in constant. One mutex,
/// written out on every mutation.
#[derive(Debug)]
pub struct AgentStore {
    path: PathBuf,
    agents: Mutex<Vec<StoredAgent>>,
}

impl AgentStore {
    /// Loads the store from `data_dir`. Never fails: a damaged document starts
    /// empty, and the built-in identity remains.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(AGENTS_FILE);

        let agents = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<AgentsFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(count = file.agents.len(), "agent store loaded");
                    file.agents
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown agent store version"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "agent store is not readable JSON");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no agent store yet; the built-in identity is the only one");
                Vec::new()
            }
            Err(err) => {
                tracing::error!(%err, "could not read the agent store");
                Vec::new()
            }
        };

        Self {
            path,
            agents: Mutex::new(agents),
        }
    }

    /// Locks the list, recovering from poison: it cannot be left torn.
    fn agents(&self) -> MutexGuard<'_, Vec<StoredAgent>> {
        self.agents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every identity: the built-in default first, then the rest by name.
    pub fn list(&self) -> Vec<Agent> {
        let agents = self.agents();

        let mut stored: Vec<Agent> = agents.iter().map(StoredAgent::to_agent).collect();
        stored.sort_by_key(|agent| agent.name.to_lowercase());

        let mut out = vec![Agent::builtin()];
        out.append(&mut stored);
        out
    }

    /// One identity by id.
    pub fn get(&self, id: &str) -> AppResult<Agent> {
        if id == DEFAULT_AGENT_ID {
            return Ok(Agent::builtin());
        }

        let agents = self.agents();
        Ok(Self::find(&agents, id)?.to_agent())
    }

    /// The identity a session runs as. Infallible: `None` is the built-in
    /// identity, an unknown id is [`Agent::stranded`] (talks, cannot act).
    pub fn resolve(&self, agent_id: Option<&str>) -> Agent {
        match agent_id {
            None | Some(DEFAULT_AGENT_ID) => Agent::builtin(),
            Some(id) => self.get(id).unwrap_or_else(|_| {
                tracing::warn!(
                    agent_id = id,
                    "a session names an identity that is not on file"
                );
                Agent::stranded(id)
            }),
        }
    }

    /// Creates an identity.
    pub fn create(&self, draft: &AgentDraft) -> AppResult<Agent> {
        let mut agents = self.agents();
        let valid = Valid::check(draft, &agents, None)?;

        let stored = valid.into_stored(&now());
        let created = stored.to_agent();

        agents.push(stored);
        self.save(&agents)?;

        tracing::info!(id = %created.id, name = %created.name, "identity created");
        Ok(created)
    }

    /// Whether an identity already answers to `name` (trimmed,
    /// case-insensitive, as [`Valid::check`] compares; PLAN 7.14).
    pub fn name_taken(&self, name: &str) -> bool {
        let name = name.trim();
        Agent::builtin().name.eq_ignore_ascii_case(name)
            || self
                .agents()
                .iter()
                .any(|agent| agent.name.eq_ignore_ascii_case(name))
    }

    /// Whether each draft would be accepted, as if they were created in order.
    ///
    /// One answer per draft; earlier drafts count against later ones. Accepted
    /// drafts come back normalized as they would be stored.
    pub fn check_all(&self, drafts: &[AgentDraft]) -> Vec<AppResult<AgentDraft>> {
        let mut scratch = self.agents().clone();
        let stamp = now();

        drafts
            .iter()
            .map(|draft| {
                let valid = Valid::check(draft, &scratch, None)?;
                let normal = valid.to_draft();
                scratch.push(valid.into_stored(&stamp));
                Ok(normal)
            })
            .collect()
    }

    /// Creates every draft or none (PLAN 7.14), validated under one lock and
    /// written once.
    pub fn create_all(&self, drafts: &[AgentDraft]) -> AppResult<Vec<Agent>> {
        let mut agents = self.agents();
        let mut next = agents.clone();
        let stamp = now();

        let mut created = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let stored = Valid::check(draft, &next, None)?.into_stored(&stamp);
            created.push(stored.to_agent());
            next.push(stored);
        }

        if created.is_empty() {
            return Ok(created);
        }
        self.save(&next)?;
        *agents = next;

        tracing::info!(count = created.len(), "identities created from a roster");
        Ok(created)
    }

    /// Replaces an identity's fields, keeping its id. Refused for the built-in
    /// identity.
    pub fn update(&self, id: &str, draft: &AgentDraft) -> AppResult<Agent> {
        if id == DEFAULT_AGENT_ID {
            return Err(AppError::AgentBuiltin { action: "edited" });
        }

        let mut agents = self.agents();
        let valid = Valid::check(draft, &agents, Some(id))?;

        let stored = Self::find_mut(&mut agents, id)?;
        stored.name = valid.name;
        stored.role = valid.role;
        stored.instructions = valid.instructions;
        stored.provider_id = valid.provider_id;
        stored.tools = valid.tools;
        stored.skills = valid.skills;
        stored.runs_per_day = valid.runs_per_day;
        stored.updated_at = now();
        let updated = stored.to_agent();

        self.save(&agents)?;
        tracing::info!(id, name = %updated.name, "identity updated");
        Ok(updated)
    }

    /// Deletes an identity. Usage checks live in
    /// [`AppState::delete_agent`](crate::AppState::delete_agent).
    pub fn delete(&self, id: &str) -> AppResult<()> {
        if id == DEFAULT_AGENT_ID {
            return Err(AppError::AgentBuiltin { action: "deleted" });
        }

        let mut agents = self.agents();
        let before = agents.len();
        agents.retain(|agent| agent.id != id);
        if agents.len() == before {
            return Err(AppError::AgentNotFound { id: id.to_owned() });
        }

        self.save(&agents)?;
        tracing::info!(id, "identity deleted");
        Ok(())
    }

    /// Looks an identity up, or reports that the caller's list is stale.
    fn find<'a>(agents: &'a [StoredAgent], id: &str) -> AppResult<&'a StoredAgent> {
        agents
            .iter()
            .find(|agent| agent.id == id)
            .ok_or_else(|| AppError::AgentNotFound { id: id.to_owned() })
    }

    /// [`AgentStore::find`], mutably.
    fn find_mut<'a>(agents: &'a mut [StoredAgent], id: &str) -> AppResult<&'a mut StoredAgent> {
        agents
            .iter_mut()
            .find(|agent| agent.id == id)
            .ok_or_else(|| AppError::AgentNotFound { id: id.to_owned() })
    }

    /// Serializes the list and replaces the document atomically.
    fn save(&self, agents: &[StoredAgent]) -> AppResult<()> {
        let file = AgentsFile {
            version: SCHEMA_VERSION,
            agents: agents.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| AppError::Store {
            action: "serialize",
            source: io::Error::other(err),
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(%err, path = %self.path.display(), "could not write the agent store");
            AppError::Store {
                action: "save",
                source: err,
            }
        })
    }

    /// Where the document lives.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A checked draft, every field in stored form.
struct Valid {
    name: String,
    role: String,
    instructions: String,
    provider_id: String,
    tools: Vec<String>,
    skills: Vec<String>,
    runs_per_day: u32,
}

impl Valid {
    /// The draft as it would be stored.
    fn to_draft(&self) -> AgentDraft {
        AgentDraft {
            name: self.name.clone(),
            role: self.role.clone(),
            instructions: self.instructions.clone(),
            provider_id: self.provider_id.clone(),
            tools: self.tools.clone(),
            skills: self.skills.clone(),
            runs_per_day: self.runs_per_day,
        }
    }

    /// The row a create writes, with a fresh id and both stamps at `stamp`.
    fn into_stored(self, stamp: &str) -> StoredAgent {
        StoredAgent {
            id: Uuid::new_v4().to_string(),
            name: self.name,
            role: self.role,
            instructions: self.instructions,
            provider_id: self.provider_id,
            tools: self.tools,
            skills: self.skills,
            runs_per_day: self.runs_per_day,
            created_at: stamp.to_owned(),
            updated_at: stamp.to_owned(),
        }
    }

    /// Checks a draft against the identities on file; `editing` is excluded
    /// from the name check. Messages say what a working value looks like.
    fn check(draft: &AgentDraft, agents: &[StoredAgent], editing: Option<&str>) -> AppResult<Self> {
        let name = draft.name.trim();
        if name.is_empty() {
            return Err(AppError::Agent {
                field: "name",
                reason: "an identity needs a name — \"Reviewer\", \"Scribe\"".to_owned(),
            });
        }
        if name.chars().count() > NAME_MAX_CHARS {
            return Err(AppError::Agent {
                field: "name",
                reason: format!("keep it under {NAME_MAX_CHARS} characters"),
            });
        }
        // Case-insensitively unique, including against the built-in one: two
        // rows reading "Reviewer" in a picker are two rows nobody can choose
        // between.
        let taken = std::iter::once(Agent::builtin().name)
            .chain(
                agents
                    .iter()
                    .filter(|agent| Some(agent.id.as_str()) != editing)
                    .map(|agent| agent.name.clone()),
            )
            .any(|existing| existing.eq_ignore_ascii_case(name));
        if taken {
            return Err(AppError::Agent {
                field: "name",
                reason: format!("`{name}` is already an identity — pick another name"),
            });
        }

        let role = draft.role.trim();
        if role.is_empty() {
            return Err(AppError::Agent {
                field: "role",
                reason: "say what this identity is for, in one line — it is what the session \
                         picker shows beside the name"
                    .to_owned(),
            });
        }
        if role.chars().count() > ROLE_MAX_CHARS {
            return Err(AppError::Agent {
                field: "role",
                reason: format!(
                    "a role is one line; keep it under {ROLE_MAX_CHARS} characters and put the \
                     detail in the instructions"
                ),
            });
        }

        let instructions = draft.instructions.trim();
        if instructions.chars().count() > INSTRUCTIONS_MAX_CHARS {
            return Err(AppError::Agent {
                field: "instructions",
                reason: format!(
                    "keep it under {INSTRUCTIONS_MAX_CHARS} characters. Instructions say what \
                     this identity is, not how to carry out a procedure — a runbook belongs in \
                     a skill"
                ),
            });
        }

        let provider_id = draft.provider_id.trim();
        if provider_id != DEFAULT_PROVIDER_ID {
            return Err(AppError::Agent {
                field: "provider",
                reason: format!(
                    "this build has one provider, `{DEFAULT_PROVIDER_ID}` — the one named in \
                     Settings. A roster to bind identities to comes later"
                ),
            });
        }

        // Registry order rather than the order the form sent, so the schemas
        // the model is shown stay in the order the registry chose: look, read,
        // then change something.
        let mut tools = Vec::new();
        for name in tools::names() {
            if draft.tools.iter().any(|granted| granted == name) {
                tools.push((*name).to_owned());
            }
        }
        // Then connector tools (Phase 18), checked for shape, not existence: a
        // connector may be down. Only connected tools are offered
        // ([`Catalog::schemas_for`](crate::mcp::Catalog::schemas_for)).
        for granted in &draft.tools {
            if tools.iter().any(|known| known == granted) {
                continue;
            }
            if connectors::split_tool_name(granted).is_none() {
                return Err(AppError::Agent {
                    field: "tools",
                    reason: format!(
                        "`{granted}` is not a tool this build has. The tools are: {}. A \
                         connector's tool is named `<connector>__<tool>`",
                        tools::names().join(", ")
                    ),
                });
            }
            tools.push(granted.clone());
        }

        let mut skills: Vec<String> = Vec::new();
        for skill in &draft.skills {
            let skill = skill.trim();
            if skill.is_empty() {
                continue;
            }
            // The same predicate discovery uses, so a name that can be
            // granted is a name a runbook's directory can have and there is no
            // third spelling in between.
            if !skills::is_name(skill) {
                return Err(AppError::Agent {
                    field: "skills",
                    reason: format!(
                        "`{skill}` is not a skill name. Use lower-case letters, digits, `.`, `-` \
                         and `_`, up to {} characters, like `inbox.triage`",
                        skills::NAME_MAX_CHARS
                    ),
                });
            }
            if !skills.iter().any(|kept| kept == skill) {
                skills.push(skill.to_owned());
            }
        }
        if skills.len() > SKILLS_MAX {
            return Err(AppError::Agent {
                field: "skills",
                reason: format!("an identity may hold at most {SKILLS_MAX} skills"),
            });
        }

        // Skills without `skill_run`/`skill_return` are refused, never
        // auto-added; the panel ticks both for the user.
        if !skills.is_empty() {
            if let Some(missing) = [tool::SKILL_RUN, tool::SKILL_RETURN]
                .iter()
                .find(|name| !tools.iter().any(|granted| granted == *name))
            {
                return Err(AppError::Agent {
                    field: "tools",
                    reason: format!(
                        "an identity that may run a skill needs `{missing}` as well — `{}` loads \
                         a runbook and `{}` records what came of it",
                        tool::SKILL_RUN,
                        tool::SKILL_RETURN
                    ),
                });
            }
        }

        if draft.runs_per_day > AGENT_RUNS_PER_DAY_MAX {
            return Err(AppError::Agent {
                field: "runs_per_day",
                reason: format!(
                    "at most {AGENT_RUNS_PER_DAY_MAX} scheduled runs a day for one identity. \
                     This is the ceiling on the role, not on any one routine — each of those \
                     has a budget of its own"
                ),
            });
        }

        Ok(Self {
            name: name.to_owned(),
            role: role.to_owned(),
            instructions: instructions.to_owned(),
            provider_id: provider_id.to_owned(),
            tools,
            skills,
            runs_per_day: draft.runs_per_day,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    use crate::error::ErrorCode;
    use crate::policy::tool;

    /// The two tools an identity granted a skill has to hold.
    fn skill_tools() -> Vec<String> {
        vec![tool::SKILL_RUN.to_owned(), tool::SKILL_RETURN.to_owned()]
    }

    /// A draft that passes, so a test can change one field and assert on that
    /// field alone.
    fn draft(name: &str) -> AgentDraft {
        AgentDraft {
            name: name.to_owned(),
            role: "reviews changes and reports what is risky".to_owned(),
            instructions: "Read before you judge.".to_owned(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            tools: vec![tool::FS_READ.to_owned(), tool::FS_LIST.to_owned()],
            skills: Vec::new(),
            runs_per_day: AGENT_RUNS_PER_DAY_DEFAULT,
        }
    }

    fn store() -> (TempDir, AgentStore) {
        let dir = TempDir::new().expect("temp dir");
        let store = AgentStore::load(dir.path());
        (dir, store)
    }

    #[test]
    fn payloads_carry_the_documented_field_names() {
        let agent = Agent::builtin();
        let json = serde_json::to_value(&agent).expect("Agent serializes");

        for key in [
            "id",
            "name",
            "role",
            "instructions",
            "provider_id",
            "tools",
            "skills",
            "builtin",
        ] {
            assert!(json.get(key).is_some(), "missing `{key}` in {json}");
        }
    }

    /// The built-in identity has to be exactly the assistant of Phases 5–11,
    /// or every session written before this phase changes behaviour.
    #[test]
    fn the_builtin_identity_holds_every_tool_and_says_nothing_extra() {
        let builtin = Agent::builtin();

        assert_eq!(builtin.id, DEFAULT_AGENT_ID);
        assert!(builtin.builtin);
        assert!(builtin.instructions.is_empty(), "no extra instructions");
        assert!(builtin.role.is_empty());
        assert_eq!(builtin.tools, tools::names());
        for name in tools::names() {
            assert!(builtin.allows(name), "{name} is granted");
        }
    }

    #[test]
    fn a_fresh_install_lists_the_builtin_identity_and_nothing_else() {
        let (_dir, store) = store();

        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, DEFAULT_AGENT_ID);
        assert!(!store.path().exists(), "reading writes no document");
    }

    #[test]
    fn an_identity_survives_a_restart() {
        let dir = TempDir::new().expect("temp dir");

        let created = AgentStore::load(dir.path())
            .create(&draft("Reviewer"))
            .expect("created");

        let reopened = AgentStore::load(dir.path());
        let found = reopened.get(&created.id).expect("still there");

        assert_eq!(found, created);
        assert_eq!(reopened.list().len(), 2, "the built-in one and this one");
    }

    /// The exit condition of the phase, at the level of the store: an identity
    /// holds the tools it was granted and no others.
    #[test]
    fn an_identity_holds_only_the_tools_it_was_granted() {
        let (_dir, store) = store();
        let reviewer = store.create(&draft("Reviewer")).expect("created");

        assert!(reviewer.allows(tool::FS_READ));
        assert!(reviewer.allows(tool::FS_LIST));
        assert!(!reviewer.allows(tool::FS_WRITE));
        assert!(!reviewer.allows(tool::SHELL_EXEC));
        assert!(!reviewer.allows(tool::SCREEN_CAPTURE));
    }

    /// Registry order, not form order: the schemas the model is shown stay in
    /// the order the registry chose.
    #[test]
    fn granted_tools_are_stored_in_registry_order() {
        let (_dir, store) = store();
        let created = store
            .create(&AgentDraft {
                tools: vec![tool::FS_WRITE.to_owned(), tool::FS_LIST.to_owned()],
                ..draft("Scribe")
            })
            .expect("created");

        assert_eq!(created.tools, vec![tool::FS_LIST, tool::FS_WRITE]);
    }

    #[test]
    fn a_tool_this_build_does_not_have_is_refused_by_name() {
        let (_dir, store) = store();

        let err = store
            .create(&AgentDraft {
                tools: vec!["net_fetch".to_owned()],
                ..draft("Fetcher")
            })
            .expect_err("refused");

        assert_eq!(err.code(), ErrorCode::InvalidSetting);
        assert!(err.to_string().contains("net_fetch"), "{err}");
        // The message names what *is* available, so the fix is on screen.
        assert!(err.to_string().contains(tool::FS_READ), "{err}");
    }

    #[test]
    fn an_identity_with_no_tools_at_all_is_allowed() {
        let (_dir, store) = store();

        let created = store
            .create(&AgentDraft {
                tools: Vec::new(),
                ..draft("Scribe")
            })
            .expect("an identity that only writes prose is a real thing to want");

        assert!(created.tools.is_empty());
        assert!(!created.allows(tool::FS_READ));
    }

    #[test]
    fn names_are_unique_case_insensitively_and_the_builtin_one_counts() {
        let (_dir, store) = store();
        store.create(&draft("Reviewer")).expect("created");

        for taken in ["Reviewer", "reviewer", "Assistant"] {
            let err = store.create(&draft(taken)).expect_err("refused");
            assert_eq!(err.code(), ErrorCode::InvalidSetting);
            assert!(err.to_string().contains(taken), "{err}");
        }
    }

    #[test]
    fn saving_an_identity_under_its_own_name_is_not_a_collision() {
        let (_dir, store) = store();
        let reviewer = store.create(&draft("Reviewer")).expect("created");

        let updated = store
            .update(
                &reviewer.id,
                &AgentDraft {
                    role: "reviews changes and files the risks in DECISIONS.md".to_owned(),
                    ..draft("Reviewer")
                },
            )
            .expect("its own name is free");

        assert_eq!(
            updated.id, reviewer.id,
            "the id is kept, so sessions stay bound"
        );
        assert!(updated.role.contains("DECISIONS.md"));
    }

    #[test]
    fn an_identity_needs_a_name_and_a_role() {
        let (_dir, store) = store();

        for (field, bad) in [
            (
                "name",
                AgentDraft {
                    name: "   ".to_owned(),
                    ..draft("x")
                },
            ),
            (
                "role",
                AgentDraft {
                    role: String::new(),
                    ..draft("Reviewer")
                },
            ),
        ] {
            let err = store.create(&bad).expect_err("refused");
            let json = serde_json::to_value(&err).expect("serializes");
            assert_eq!(json["field"], field, "{json}");
        }
    }

    /// The prompt stays a policy summary plus what is true now (PLAN 7.1). An
    /// identity that has grown a runbook is procedure paid for on every turn;
    /// a skill is the same procedure paid for on the turns that use it.
    #[test]
    fn instructions_are_capped_and_the_message_says_where_a_runbook_goes() {
        let (_dir, store) = store();

        let err = store
            .create(&AgentDraft {
                instructions: "x".repeat(INSTRUCTIONS_MAX_CHARS + 1),
                ..draft("Runbook")
            })
            .expect_err("refused");

        assert!(err.to_string().contains("skill"), "{err}");
    }

    #[test]
    fn only_the_one_provider_this_build_has_can_be_bound() {
        let (_dir, store) = store();

        let err = store
            .create(&AgentDraft {
                provider_id: "anthropic".to_owned(),
                ..draft("Reviewer")
            })
            .expect_err("refused");

        let json = serde_json::to_value(&err).expect("serializes");
        assert_eq!(json["field"], "provider");
        assert!(err.to_string().contains(DEFAULT_PROVIDER_ID), "{err}");
    }

    #[test]
    fn skill_names_are_the_ones_a_runbook_directory_can_have() {
        let (_dir, store) = store();

        let mut granted = draft("Triager");
        granted.tools.extend(skill_tools());

        let created = store
            .create(&AgentDraft {
                skills: vec![
                    "inbox.triage".to_owned(),
                    "  ".to_owned(),
                    "inbox.triage".to_owned(),
                ],
                ..granted.clone()
            })
            .expect("created");
        assert_eq!(
            created.skills,
            vec!["inbox.triage"],
            "blanks dropped, duplicates collapsed"
        );
        assert!(created.allows_skill("inbox.triage"));
        assert!(!created.allows_skill("deploy.draft"));

        let err = store
            .create(&AgentDraft {
                name: "Other".to_owned(),
                skills: vec!["Inbox Triage".to_owned()],
                ..granted
            })
            .expect_err("refused");
        assert!(err.to_string().contains("inbox.triage"), "{err}");
    }

    /// A skill an identity cannot load is a grant that does nothing, and the
    /// refusal says which tools to tick rather than saving it silently.
    #[test]
    fn granting_a_skill_without_the_tools_that_run_one_is_refused() {
        let (_dir, store) = store();

        let err = store
            .create(&AgentDraft {
                skills: vec!["inbox.triage".to_owned()],
                ..draft("Triager")
            })
            .expect_err("refused");

        let json = serde_json::to_value(&err).expect("serializes");
        assert_eq!(json["field"], "tools", "the form marks the tick-list");
        assert!(err.to_string().contains(tool::SKILL_RUN), "{err}");
        assert!(err.to_string().contains(tool::SKILL_RETURN), "{err}");
    }

    /// The built-in identity holds every tool and no skill: it is the
    /// assistant from before either allow-list existed, and that assistant had
    /// no runbooks.
    #[test]
    fn the_builtin_identity_runs_no_skill() {
        let builtin = Agent::builtin();

        assert!(builtin.skills.is_empty());
        assert!(!builtin.allows_skill("inbox.triage"));
        assert!(builtin.allows(tool::SKILL_RUN), "it can still load one");
    }

    #[test]
    fn the_builtin_identity_can_be_neither_edited_nor_deleted() {
        let (_dir, store) = store();

        let edited = store
            .update(DEFAULT_AGENT_ID, &draft("Reviewer"))
            .expect_err("refused");
        assert!(edited.to_string().contains("edited"), "{edited}");

        let deleted = store.delete(DEFAULT_AGENT_ID).expect_err("refused");
        assert!(deleted.to_string().contains("deleted"), "{deleted}");

        assert_eq!(store.list().len(), 1, "still there");
    }

    #[test]
    fn deleting_removes_only_the_named_identity() {
        let (_dir, store) = store();
        let reviewer = store.create(&draft("Reviewer")).expect("created");
        let scribe = store
            .create(&AgentDraft { ..draft("Scribe") })
            .expect("created");

        store.delete(&reviewer.id).expect("deleted");

        assert!(store.get(&reviewer.id).is_err());
        assert!(store.get(&scribe.id).is_ok());
        assert!(
            store.delete(&reviewer.id).is_err(),
            "a stale list is reported, not ignored"
        );
    }

    #[test]
    fn a_session_that_named_nothing_resolves_to_the_builtin_identity() {
        let (_dir, store) = store();

        assert_eq!(store.resolve(None), Agent::builtin());
        assert_eq!(store.resolve(Some(DEFAULT_AGENT_ID)), Agent::builtin());
    }

    /// Only reachable by hand-editing the document. The failure has to be
    /// narrow, not wide: a damaged file must never widen what a session can do.
    #[test]
    fn a_session_naming_an_identity_that_is_gone_can_talk_but_not_act() {
        let (_dir, store) = store();

        let stranded = store.resolve(Some("2f1c-not-on-file"));

        assert_eq!(stranded.id, "2f1c-not-on-file");
        assert!(stranded.tools.is_empty(), "no tools, not every tool");
        for name in tools::names() {
            assert!(!stranded.allows(name), "{name} is not granted");
        }
    }

    #[test]
    fn a_damaged_document_is_moved_aside_instead_of_blocking_startup() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(AGENTS_FILE);
        fs::write(&path, b"{ this is not json").expect("write");

        let store = AgentStore::load(dir.path());

        assert_eq!(store.list().len(), 1, "the built-in identity still answers");
        assert!(!path.exists(), "the damaged document was moved aside");
        assert!(
            fs::read_dir(dir.path())
                .expect("read dir")
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-")),
            "and kept, rather than deleted"
        );
    }

    #[test]
    fn a_future_schema_version_is_quarantined_rather_than_guessed_at() {
        let dir = TempDir::new().expect("temp dir");
        fs::write(
            dir.path().join(AGENTS_FILE),
            br#"{"version":99,"agents":[]}"#,
        )
        .expect("write");

        let store = AgentStore::load(dir.path());
        assert_eq!(store.list().len(), 1);
        assert!(!dir.path().join(AGENTS_FILE).exists());
    }

    #[test]
    fn a_hand_edited_document_with_a_byte_order_mark_still_loads() {
        let dir = TempDir::new().expect("temp dir");
        let created = AgentStore::load(dir.path())
            .create(&draft("Reviewer"))
            .expect("created");

        let path = dir.path().join(AGENTS_FILE);
        let body = fs::read(&path).expect("read");
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(&body);
        fs::write(&path, with_bom).expect("write");

        assert!(AgentStore::load(dir.path()).get(&created.id).is_ok());
    }

    /// A roster applied halfway is a cabinet nobody signed (PLAN 7.14).
    #[test]
    fn creating_a_batch_writes_every_identity_or_none() {
        let (_dir, store) = store();

        let refused = store
            .create_all(&[
                draft("Chief"),
                AgentDraft {
                    tools: vec!["net_fetch".to_owned()],
                    ..draft("Fetcher")
                },
            ])
            .expect_err("refused");
        assert!(refused.to_string().contains("net_fetch"), "{refused}");
        assert_eq!(store.list().len(), 1, "the Chief was not created either");

        let created = store
            .create_all(&[draft("Chief"), draft("Reviewer")])
            .expect("created");
        assert_eq!(created.len(), 2);
        assert_eq!(
            AgentStore::load(store.path().parent().expect("dir"))
                .list()
                .len(),
            3
        );
    }

    #[test]
    fn a_batch_is_checked_against_itself_as_well_as_the_document() {
        let (_dir, store) = store();
        store.create(&draft("Reviewer")).expect("created");

        let checked = store.check_all(&[draft("Chief"), draft("chief"), draft("REVIEWER")]);

        assert!(checked[0].is_ok());
        assert!(checked[1].is_err(), "the batch's own first Chief counts");
        assert!(checked[2].is_err(), "the document's Reviewer counts");
        assert_eq!(store.list().len(), 2, "checking writes nothing");
    }

    #[test]
    fn a_name_is_taken_case_insensitively_and_the_builtin_one_counts() {
        let (_dir, store) = store();
        store.create(&draft("Reviewer")).expect("created");

        assert!(store.name_taken(" reviewer "));
        assert!(store.name_taken("assistant"));
        assert!(!store.name_taken("Chief of Staff"));
    }

    #[test]
    fn builtin_is_never_written_to_disk() {
        let dir = TempDir::new().expect("temp dir");
        AgentStore::load(dir.path())
            .create(&draft("Reviewer"))
            .expect("created");

        let document = fs::read_to_string(dir.path().join(AGENTS_FILE)).expect("read");
        assert!(!document.contains("builtin"), "{document}");
    }
}
