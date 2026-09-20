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
use crate::store::{connectors, settings};
use crate::tools;

/// Name of the document under the application-data directory.
const AGENTS_FILE: &str = "agents.json";

/// Schema version of [`AgentsFile`], independent of the other documents.
const SCHEMA_VERSION: u32 = 1;

/// The identity a session runs as when it named none. Reserved: no stored
/// agent carries it.
pub const DEFAULT_AGENT_ID: &str = "default";

/// The provider row that always exists (PLAN 7.19). The built-in identity and
/// every applied roster answer from it.
pub const DEFAULT_PROVIDER_ID: &str = "default";

/// Whether a provider id is on file. The agent store cannot see the settings
/// document; [`AppState`](crate::AppState) passes the roster in.
pub type KnownProvider<'a> = &'a dyn Fn(&str) -> bool;

/// The roster as a store without settings sees it: the default row only.
fn only_default(id: &str) -> bool {
    id == DEFAULT_PROVIDER_ID
}

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
    /// Which provider row answers for it (PLAN 7.19).
    pub provider_id: String,
    /// The model it sends; empty uses the row's.
    pub model: String,
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
            model: String::new(),
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
            model: String::new(),
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
    /// Which provider row answers for it. Must be on file.
    pub provider_id: String,
    /// The model it sends; empty uses the row's. Defaulted for older callers.
    #[serde(default)]
    pub model: String,
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
    #[serde(default)]
    model: String,
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
            model: self.model.clone(),
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

    /// Creates an identity bound to the default provider row, or refuses.
    pub fn create(&self, draft: &AgentDraft) -> AppResult<Agent> {
        self.create_with(draft, &only_default)
    }

    /// Creates an identity whose provider `known` accepts.
    pub fn create_with(&self, draft: &AgentDraft, known: KnownProvider<'_>) -> AppResult<Agent> {
        let mut agents = self.agents();
        let valid = Valid::check(draft, &agents, None, known)?;

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
                let valid = Valid::check(draft, &scratch, None, &only_default)?;
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
            let stored = Valid::check(draft, &next, None, &only_default)?.into_stored(&stamp);
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

    /// Replaces an identity's fields, keeping its id, with the default row as
    /// the only provider. Refused for the built-in identity.
    pub fn update(&self, id: &str, draft: &AgentDraft) -> AppResult<Agent> {
        self.update_with(id, draft, &only_default)
    }

    /// Replaces an identity's fields, with any provider `known` accepts.
    pub fn update_with(
        &self,
        id: &str,
        draft: &AgentDraft,
        known: KnownProvider<'_>,
    ) -> AppResult<Agent> {
        if id == DEFAULT_AGENT_ID {
            return Err(AppError::AgentBuiltin { action: "edited" });
        }

        let mut agents = self.agents();
        let valid = Valid::check(draft, &agents, Some(id), known)?;

        let stored = Self::find_mut(&mut agents, id)?;
        stored.name = valid.name;
        stored.role = valid.role;
        stored.instructions = valid.instructions;
        stored.provider_id = valid.provider_id;
        stored.model = valid.model;
        stored.tools = valid.tools;
        stored.skills = valid.skills;
        stored.runs_per_day = valid.runs_per_day;
        stored.updated_at = now();
        let updated = stored.to_agent();

        self.save(&agents)?;
        tracing::info!(id, name = %updated.name, "identity updated");
        Ok(updated)
    }

    /// How many stored identities answer from `provider_id`.
    pub fn count_for_provider(&self, provider_id: &str) -> usize {
        self.agents()
            .iter()
            .filter(|agent| agent.provider_id == provider_id)
            .count()
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
    model: String,
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
            model: self.model.clone(),
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
            model: self.model,
            tools: self.tools,
            skills: self.skills,
            runs_per_day: self.runs_per_day,
            created_at: stamp.to_owned(),
            updated_at: stamp.to_owned(),
        }
    }

    /// Checks a draft against the identities on file; `editing` is excluded
    /// from the name check. Messages say what a working value looks like.
    fn check(
        draft: &AgentDraft,
        agents: &[StoredAgent],
        editing: Option<&str>,
        known: KnownProvider<'_>,
    ) -> AppResult<Self> {
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
        if !known(provider_id) {
            return Err(AppError::Agent {
                field: "provider",
                reason: format!(
                    "`{provider_id}` is not a provider in Settings — pick one from the list, \
                     or `{DEFAULT_PROVIDER_ID}`"
                ),
            });
        }
        let model = settings::normalize_model(&draft.model).map_err(|_| AppError::Agent {
            field: "model",
            reason: "a model id has no spaces in it; leave it empty to use the provider's"
                .to_owned(),
        })?;

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
            model,
            tools,
            skills,
            runs_per_day: draft.runs_per_day,
        })
    }
}

#[cfg(test)]
mod tests;
