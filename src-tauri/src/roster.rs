//! Cabinet founding: a roster proposal, then apply (PLAN 7.14).
//!
//! A session running `cabinet.found` writes [`ROSTER_FILE`]; a person applies it
//! in Settings, creating the identities it names. Unlike a skill proposal, this
//! *is* a grant, so the preview shows each allow-list and apply signs them.
//!
//! * **Parsed, no model** ([`parse`]): `## Name` plus `role`, `tools`, `skills`,
//!   `runs_per_day`, validated as [`AgentDraft`]s.
//! * **No tool**: [`apply`] is reachable only from a command.
//! * **What was shown is what is created**: apply checks the preview's digest.
//! * **All or nothing** ([`AgentStore::create_all`]).
//! * **Existing names are skipped, never widened.**
//! * **Only identities**: no routines, connectors or `world/`.
//! * **Connector tools must be live** to be newly granted.
//!
//! The proposal travels with the project; the identities are application data.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use crate::audit::{AuditDecision, AuditEntry, AuditLog, AuditRecord, Outcome};
use crate::error::{AppError, AppResult};
use crate::policy::path;
use crate::skills::{self, Skill};
use crate::store::connectors;
use crate::store::{Agent, AgentDraft, AgentStore, DEFAULT_PROVIDER_ID};
use crate::tools;

/// Where a roster proposal lives in a workspace's cabinet.
pub const ROSTER_DIR: &str = ".aegis/roster";

/// The roster proposal itself.
///
/// Named `PROPOSAL.md` because it is never in force; apply writes identity rows
/// and leaves it in place.
pub const ROSTER_FILE: &str = ".aegis/roster/PROPOSAL.md";

/// The name an apply's lines carry in the audit log's `tool` column.
///
/// Not a callable tool: one line per identity a person's apply created.
pub const AGENT_CREATE: &str = "agent_create";

/// Largest roster the parser reads (the runbook cap).
pub const ROSTER_MAX_BYTES: usize = 16 * 1024;

/// Most identities one roster may propose.
pub const ROSTER_MAX: usize = 16;

/// The fields every identity names, in the order a roster writes them.
const KEYS: [&str; 4] = ["role", "tools", "skills", "runs_per_day"];

/// The section that lists clocks somebody intends, and creates none.
const ROUTINES_HEADING: &str = "Intended routines";

/// The section that lists what the founder could not settle.
const QUESTIONS_HEADING: &str = "Open questions";

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

/// A roster that parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterDoc {
    /// Every identity proposed, in file order.
    pub identities: Vec<Proposed>,
    /// The `## Intended routines` bullets, as written.
    pub intended_routines: Vec<String>,
    /// The `## Open questions` bullets, as written.
    pub open_questions: Vec<String>,
}

/// One identity as a roster names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    /// The heading.
    pub name: String,
    /// `role:`.
    pub role: String,
    /// `tools:`, as written. Validated at preview, not here.
    pub tools: Vec<String>,
    /// `skills:`, as written.
    pub skills: Vec<String>,
    /// `runs_per_day:`.
    pub runs_per_day: u32,
}

/// Parses and judges a roster proposal.
///
/// Strict, since no model fills gaps; refusals name the identity and line.
///
/// * A `# ` title and prose before the first `## ` are ignored.
/// * `## Name`, then `- key: value` for each of the [`KEYS`] exactly once
///   (comma lists, `none` for empty). Lines without a dash are prose and grant
///   nothing.
/// * `## Intended routines` and `## Open questions` are listed as written.
pub fn parse(text: &str) -> Result<RosterDoc, String> {
    if text.len() > ROSTER_MAX_BYTES {
        return Err(format!(
            "this roster is {} bytes and apply reads at most {ROSTER_MAX_BYTES}. A roster is a \
             handful of identities with four fields each",
            text.len()
        ));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let mut section = Section::Preamble;
    let mut identities: Vec<Fields> = Vec::new();
    let mut intended_routines = Vec::new();
    let mut open_questions = Vec::new();

    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            let heading = heading.trim();
            section = if heading.eq_ignore_ascii_case(ROUTINES_HEADING) {
                Section::Routines
            } else if heading.eq_ignore_ascii_case(QUESTIONS_HEADING) {
                Section::Questions
            } else {
                if heading.is_empty() {
                    return Err(
                        "a `## ` heading has no name. Each identity is `## ` and its name, like \
                         `## Reviewer`"
                            .to_owned(),
                    );
                }
                if identities
                    .iter()
                    .any(|fields| fields.name.eq_ignore_ascii_case(heading))
                {
                    return Err(format!(
                        "`## {heading}` is proposed twice. One name is one identity — merge them"
                    ));
                }
                identities.push(Fields::new(heading));
                Section::Identity
            };
            continue;
        }

        let trimmed = line.trim();
        let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .map(str::trim)
        else {
            continue;
        };

        match section {
            Section::Preamble => {}
            Section::Routines | Section::Questions => {
                if !item.is_empty() && !item.eq_ignore_ascii_case("none") {
                    let list = if section == Section::Routines {
                        &mut intended_routines
                    } else {
                        &mut open_questions
                    };
                    list.push(item.to_owned());
                }
            }
            Section::Identity => {
                if let Some(fields) = identities.last_mut() {
                    fields.set(item)?;
                }
            }
        }
    }

    if identities.is_empty() {
        return Err(
            "this roster names no identity. Each one is a `## Name` heading with `role`, `tools`, \
             `skills` and `runs_per_day` under it"
                .to_owned(),
        );
    }
    if identities.len() > ROSTER_MAX {
        return Err(format!(
            "this roster proposes {} identities, and a roster holds at most {ROSTER_MAX}. A \
             cabinet is a Chief, a Reviewer and a specialist per domain",
            identities.len()
        ));
    }

    Ok(RosterDoc {
        identities: identities
            .into_iter()
            .map(Fields::finish)
            .collect::<Result<_, _>>()?,
        intended_routines,
        open_questions,
    })
}

/// Which part of the file a line is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Preamble,
    Identity,
    Routines,
    Questions,
}

/// One identity's fields while the file is being read.
struct Fields {
    name: String,
    role: Option<String>,
    tools: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    runs_per_day: Option<u32>,
}

impl Fields {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            role: None,
            tools: None,
            skills: None,
            runs_per_day: None,
        }
    }

    /// Reads one `- key: value` line.
    fn set(&mut self, item: &str) -> Result<(), String> {
        let name = self.name.clone();
        let Some((key, value)) = item.split_once(':') else {
            return Err(format!(
                "under `## {name}`, `- {item}` is not a field. A field is `- key: value`, and the \
                 keys are {}. Prose goes on a line without a dash",
                KEYS.join(", ")
            ));
        };
        let key = key
            .trim()
            .trim_matches(|c| c == '`' || c == '*')
            .to_ascii_lowercase();
        let value = value.trim();

        match key.as_str() {
            "role" => once(&mut self.role, value.to_owned(), &name, &key),
            "tools" => once(&mut self.tools, list(value), &name, &key),
            "skills" => once(&mut self.skills, list(value), &name, &key),
            "runs_per_day" => {
                let raw = value.trim_matches('`');
                let runs = raw.parse::<u32>().map_err(|_| {
                    format!(
                        "`## {name}` has `runs_per_day: {raw}`, which is not a whole number. `0` \
                         is an identity nothing may schedule"
                    )
                })?;
                once(&mut self.runs_per_day, runs, &name, &key)
            }
            other => Err(format!(
                "under `## {name}`, `{other}` is not a roster field. The fields are {}",
                KEYS.join(", ")
            )),
        }
    }

    /// The identity, once every field was given.
    fn finish(self) -> Result<Proposed, String> {
        let name = self.name;
        let missing = |key: &str| {
            format!(
                "`## {name}` has no `{key}:`. Every identity names all four — {} — so that \
                 nothing it holds is a default nobody wrote. An empty list is `none`",
                KEYS.join(", ")
            )
        };

        Ok(Proposed {
            role: self.role.ok_or_else(|| missing("role"))?,
            tools: self.tools.ok_or_else(|| missing("tools"))?,
            skills: self.skills.ok_or_else(|| missing("skills"))?,
            runs_per_day: self.runs_per_day.ok_or_else(|| missing("runs_per_day"))?,
            name,
        })
    }
}

/// Fills a field that has not been given yet.
fn once<T>(slot: &mut Option<T>, value: T, name: &str, key: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!(
            "`## {name}` gives `{key}` twice. Say it once — apply would have to pick one, and it \
             does not guess"
        ));
    }
    *slot = Some(value);
    Ok(())
}

/// A comma-separated list. `none`, blanks and backticks are dropped.
fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().trim_matches('`').trim())
        .filter(|item| !item.is_empty() && !item.eq_ignore_ascii_case("none"))
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// The preview (IPC payloads)
// ---------------------------------------------------------------------------

/// What apply would do with one proposed identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum RosterEntryState {
    /// No identity answers to the name: apply creates it.
    New,
    /// One does. Apply skips it and leaves its allow-lists as they are.
    Present,
    /// The name is the built-in Assistant's, which apply never touches.
    Builtin,
}

/// One proposed identity, as Settings previews it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct RosterEntry {
    /// The name it would be created under.
    pub name: String,
    /// What it is for.
    pub role: String,
    /// The tools it would hold, as the roster names them.
    pub tools: Vec<String>,
    /// The runbooks it would hold.
    pub skills: Vec<String>,
    /// Its ceiling on scheduled runs.
    pub runs_per_day: u32,
    /// What apply would do with it.
    pub state: RosterEntryState,
    /// Why it cannot be created as proposed — which refuses the whole apply.
    /// Only a `new` entry is judged: the others are not written.
    pub problem: Option<String>,
    /// What is true of it and does not block: a runbook it would hold and could
    /// not run, or one that is not on this machine yet.
    pub notes: Vec<String>,
}

/// A workspace's roster proposal, judged against the identities on file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct RosterProposal {
    /// The `PROPOSAL.md` itself.
    pub path: String,
    /// SHA-256 of the file this preview was built from, hex. Apply is refused
    /// unless it is handed back and still matches.
    pub digest: String,
    /// Every identity it proposes, in file order. Empty when it will not parse.
    pub entries: Vec<RosterEntry>,
    /// Clocks the founder intends. Apply creates none.
    pub intended_routines: Vec<String>,
    /// What the founder could not settle.
    pub open_questions: Vec<String>,
    /// Why it will not parse, when it will not.
    pub problem: Option<String>,
    /// Whether apply would create at least one identity and refuse none.
    pub appliable: bool,
}

/// What an apply did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct RosterApplied {
    /// The identities created, as stored.
    pub created: Vec<Agent>,
    /// The names that already existed and were left alone.
    pub skipped: Vec<String>,
}

impl RosterEntry {
    /// The draft apply creates this entry from, with no instructions (those are
    /// a person's to write).
    fn draft(&self) -> AgentDraft {
        AgentDraft {
            name: self.name.clone(),
            role: self.role.clone(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            model: String::new(),
            tools: self.tools.clone(),
            skills: self.skills.clone(),
            runs_per_day: self.runs_per_day,
        }
    }
}

/// This workspace's roster proposal, or `None` when it has none.
///
/// `live` is the callable connector tools; `catalog` the runbooks, for notes.
/// Never fails: an unreadable file is a proposal carrying its problem.
pub fn read(
    root: &Path,
    agents: &AgentStore,
    live: &[String],
    catalog: &[Skill],
) -> Option<RosterProposal> {
    let at = match locate(root) {
        Ok(Some(at)) => at,
        Ok(None) => return None,
        Err(problem) => return Some(broken(&root.join(ROSTER_FILE), String::new(), problem)),
    };

    let bytes = match fs::read(&at) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Some(broken(
                &at,
                String::new(),
                format!("this file could not be read: {err}"),
            ))
        }
    };
    let digest = digest(&bytes);

    let parsed = String::from_utf8(bytes)
        .map_err(|_| "`PROPOSAL.md` has to be UTF-8 text".to_owned())
        .and_then(|text| parse(&text));

    Some(match parsed {
        Ok(doc) => evaluate(&at, digest, doc, agents, live, catalog),
        Err(problem) => broken(&at, digest, problem),
    })
}

/// The proposal's path, or `None` when there is no file there.
///
/// Resolved with containment, so a linked-away `.aegis/roster` is refused.
fn locate(root: &Path) -> Result<Option<PathBuf>, String> {
    let resolved = path::resolve(root, ROSTER_FILE).map_err(|err| err.reason().to_owned())?;
    if !resolved.inside || resolved.escaped() {
        return Err(format!(
            "`{ROSTER_DIR}` points outside this workspace, and a roster read from somewhere else \
             is never applied"
        ));
    }
    Ok(resolved.path.is_file().then_some(resolved.path))
}

/// A proposal that cannot be applied at all.
fn broken(at: &Path, digest: String, problem: String) -> RosterProposal {
    RosterProposal {
        path: at.display().to_string(),
        digest,
        entries: Vec::new(),
        intended_routines: Vec::new(),
        open_questions: Vec::new(),
        problem: Some(problem),
        appliable: false,
    }
}

/// Judges a parsed roster against what is on file.
fn evaluate(
    at: &Path,
    digest: String,
    doc: RosterDoc,
    agents: &AgentStore,
    live: &[String],
    catalog: &[Skill],
) -> RosterProposal {
    let builtin = Agent::builtin().name;

    let mut entries: Vec<RosterEntry> = doc
        .identities
        .iter()
        .map(|proposed| {
            let state = if builtin.eq_ignore_ascii_case(proposed.name.trim()) {
                RosterEntryState::Builtin
            } else if agents.name_taken(&proposed.name) {
                RosterEntryState::Present
            } else {
                RosterEntryState::New
            };
            let new = state == RosterEntryState::New;

            RosterEntry {
                name: proposed.name.clone(),
                role: proposed.role.clone(),
                tools: proposed.tools.clone(),
                skills: proposed.skills.clone(),
                runs_per_day: proposed.runs_per_day,
                state,
                problem: if new {
                    dead_connector(&proposed.tools, live)
                } else {
                    None
                },
                notes: if new {
                    notes(proposed, catalog)
                } else {
                    Vec::new()
                },
            }
        })
        .collect();

    // The identity form's validator over the batch; accepted entries take the
    // normalized lists. One reason per identity.
    let fresh: Vec<usize> = (0..entries.len())
        .filter(|&index| entries[index].state == RosterEntryState::New)
        .collect();
    let drafts: Vec<AgentDraft> = fresh.iter().map(|&index| entries[index].draft()).collect();
    for (&index, checked) in fresh.iter().zip(agents.check_all(&drafts)) {
        let entry = &mut entries[index];
        match checked {
            Ok(normal) => {
                entry.role = normal.role;
                entry.tools = normal.tools;
                entry.skills = normal.skills;
            }
            Err(err) => {
                entry.problem.get_or_insert_with(|| err.to_string());
            }
        }
    }

    let appliable = entries.iter().all(|entry| entry.problem.is_none())
        && entries
            .iter()
            .any(|entry| entry.state == RosterEntryState::New);

    RosterProposal {
        path: at.display().to_string(),
        digest,
        entries,
        intended_routines: doc.intended_routines,
        open_questions: doc.open_questions,
        problem: None,
        appliable,
    }
}

/// The first connector tool no running connector offers, as a refusal.
fn dead_connector(granted: &[String], live: &[String]) -> Option<String> {
    granted
        .iter()
        .find(|name| {
            !tools::names().contains(&name.as_str())
                && connectors::split_tool_name(name).is_some()
                && !live.contains(name)
        })
        .map(|name| {
            format!(
                "`{name}` is not offered by a connector that is running. Installing and starting \
                 one is yours, in Settings → Connectors; apply grants no tool nothing answers to"
            )
        })
}

/// What is true of a proposed identity's runbooks and does not block apply.
///
/// E.g. a runbook declaring a tool the identity lacks: warned, not refused.
fn notes(proposed: &Proposed, catalog: &[Skill]) -> Vec<String> {
    proposed
        .skills
        .iter()
        .filter_map(|name| {
            let Some(skill) = skills::find(catalog, name) else {
                return Some(format!(
                    "`{name}` is not in the library or this workspace yet. The grant is kept, and \
                     does nothing until a runbook of that name exists"
                ));
            };
            let missing: Vec<String> = skill
                .tools
                .iter()
                .filter(|wanted| !proposed.tools.contains(wanted))
                .map(|wanted| format!("`{wanted}`"))
                .collect();
            (!missing.is_empty()).then(|| {
                format!(
                    "`{name}` calls {}, which this identity would not hold, so every run of it \
                     would stop before its first step",
                    missing.join(", ")
                )
            })
        })
        .collect()
}

/// SHA-256 of the file, hex.
fn digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

/// Applies this workspace's roster proposal: the grant (PLAN 7.14).
///
/// Re-reads and re-judges the file, refusing if it no longer matches `digest`.
/// Creates every `new` identity or none, with one operator audit line each
/// (returned for announcement).
pub fn apply(
    root: &Path,
    agents: &AgentStore,
    live: &[String],
    catalog: &[Skill],
    digest: &str,
    audit: &AuditLog,
    project_id: &str,
) -> AppResult<(RosterApplied, Vec<AuditEntry>)> {
    let refuse = |reason: String| AppError::Roster { reason };

    let Some(proposal) = read(root, agents, live, catalog) else {
        return Err(refuse(format!(
            "there is no `{ROSTER_FILE}` in this workspace any more"
        )));
    };
    if proposal.digest != digest {
        return Err(refuse(
            "`PROPOSAL.md` changed after it was shown. Read it again — what you confirm is what \
             is created"
                .to_owned(),
        ));
    }
    if let Some(problem) = proposal.problem {
        return Err(refuse(format!(
            "it will not parse, and a roster that does not parse is never applied: {problem}"
        )));
    }
    if let Some((name, problem)) = proposal
        .entries
        .iter()
        .find_map(|entry| Some((&entry.name, entry.problem.as_ref()?)))
    {
        return Err(refuse(format!(
            "`{name}` cannot be created as proposed: {problem}. Nothing was created"
        )));
    }

    let (fresh, skipped): (Vec<&RosterEntry>, Vec<&RosterEntry>) = proposal
        .entries
        .iter()
        .partition(|entry| entry.state == RosterEntryState::New);
    let drafts: Vec<AgentDraft> = fresh.iter().map(|entry| entry.draft()).collect();

    let created = agents.create_all(&drafts)?;

    let signed = proposal.digest.get(..12).unwrap_or(&proposal.digest);
    let lines = created
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let args = json!({
                "project_id": project_id,
                "path": ROSTER_FILE,
                "agent_id": agent.id,
                "name": agent.name,
                "tools": agent.tools,
                "skills": agent.skills,
                "runs_per_day": agent.runs_per_day,
            });
            audit.append(&AuditRecord {
                session_id: "",
                agent_id: "",
                turn_id: "",
                call_id: &format!("roster:{signed}:{index}"),
                tool: AGENT_CREATE,
                skill: "",
                handoff: "",
                routine: "",
                decision: AuditDecision::Operator,
                policy_reason: "applied from a roster proposal in Settings: a person confirmed \
                                these allow-lists as shown, and names already on file were skipped",
                args: &args,
                outcome: Outcome::Ok,
                duration_ms: 0,
                bytes_in: 0,
                bytes_out: 0,
                error_code: None,
                artifact: None,
            })
        })
        .collect();

    tracing::info!(
        created = created.len(),
        skipped = skipped.len(),
        "a roster proposal was applied"
    );

    Ok((
        RosterApplied {
            created,
            skipped: skipped
                .into_iter()
                .map(|entry| entry.name.clone())
                .collect(),
        },
        lines,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    use crate::policy::tool;

    const CHIEF: &str = "## Chief of Staff\n\n\
        - role: routes work to specialists and keeps the board\n\
        - tools: fs_list, fs_read, fs_write, skill_run, skill_return, handoff_delegate\n\
        - skills: cos.loop\n\
        - runs_per_day: 0\n\n";

    const REVIEWER: &str = "## Reviewer\n\n\
        - role: reads what the cabinet produced and says what is wrong with it\n\
        - tools: fs_list, fs_read\n\
        - skills: none\n\
        - runs_per_day: 0\n\n";

    fn roster(body: &str) -> String {
        format!("# Roster\n\nRouting for this project.\n\n{body}")
    }

    struct Fixture {
        _dir: TempDir,
        root: PathBuf,
        data: PathBuf,
        agents: AgentStore,
        audit: AuditLog,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            let data = dir.path().join("data");
            let root = dir.path().join("work");
            fs::create_dir_all(&data).expect("data dir");
            fs::create_dir_all(root.join(ROSTER_DIR)).expect("roster dir");
            let root = dunce::canonicalize(&root).expect("canonical");

            Self {
                agents: AgentStore::load(&data),
                audit: AuditLog::new(&data),
                data,
                root,
                _dir: dir,
            }
        }

        fn propose(&self, text: &str) {
            fs::write(self.root.join(ROSTER_FILE), text).expect("proposal written");
        }

        fn shown(&self, live: &[String]) -> RosterProposal {
            read(&self.root, &self.agents, live, &[]).expect("a proposal")
        }

        fn apply_shown(&self, live: &[String]) -> AppResult<(RosterApplied, Vec<AuditEntry>)> {
            let digest = self.shown(live).digest;
            apply(
                &self.root,
                &self.agents,
                live,
                &[],
                &digest,
                &self.audit,
                "p1",
            )
        }

        fn made(&self, name: &str, tools: &[&str]) -> Agent {
            self.agents
                .create(&AgentDraft {
                    name: name.to_owned(),
                    role: "made by hand".to_owned(),
                    instructions: String::new(),
                    provider_id: DEFAULT_PROVIDER_ID.to_owned(),
                    model: String::new(),
                    tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
                    skills: Vec::new(),
                    runs_per_day: 0,
                })
                .expect("created")
        }
    }

    /// The indented roster inside `cabinet.found`'s steps, de-indented.
    fn founder_example() -> String {
        skills::FOUND_SEED
            .lines()
            .skip_while(|line| *line != "    # Roster")
            .take_while(|line| line.is_empty() || line.starts_with("    "))
            .map(|line| line.strip_prefix("    ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The runbook is the format's documentation. If its example stops
    /// parsing, every roster a founder writes from it is refused.
    #[test]
    fn the_example_in_the_founder_runbook_is_the_default_roster_and_parses() {
        let doc = parse(&founder_example()).expect("the example parses");

        let names: Vec<&str> = doc.identities.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Chief of Staff", "Reviewer"]);

        let chief = &doc.identities[0];
        assert_eq!(chief.runs_per_day, 0, "never on a clock");
        assert!(!chief.tools.contains(&tool::SHELL_EXEC.to_owned()));
        assert!(!chief.tools.contains(&tool::SCREEN_CAPTURE.to_owned()));
        assert!(!chief.tools.contains(&tool::HANDOFF_RETURN.to_owned()));
        assert!(chief.skills.contains(&skills::COS_SKILL.to_owned()));

        let reviewer = &doc.identities[1];
        for refused in [tool::FS_WRITE, tool::SHELL_EXEC, tool::HANDOFF_DELEGATE] {
            assert!(!reviewer.tools.contains(&refused.to_owned()), "{refused}");
        }
        assert!(doc.intended_routines.is_empty(), "`- none` is no routine");
    }

    /// The default Chief holds every tool its runbooks declare, so nothing it
    /// is proposed with fails closed on its first run.
    #[test]
    fn the_default_roster_holds_every_tool_its_runbooks_call() {
        let dir = TempDir::new().expect("temp dir");
        let library = dir.path().join(skills::LIBRARY_DIR);
        skills::seed(&library);
        let catalog = skills::catalog(&library, None);

        for proposed in parse(&founder_example()).expect("parses").identities {
            assert_eq!(
                notes(&proposed, &catalog),
                Vec::<String>::new(),
                "{}",
                proposed.name
            );
        }
    }

    #[test]
    fn every_identity_names_all_four_fields_and_the_refusal_says_which() {
        for key in KEYS {
            let text = roster(CHIEF).replace(&format!("- {key}:"), "- dropped:");
            let text = text.replace("- dropped:", "Dropped, as prose:");
            let err = parse(&text).expect_err("refused");
            assert!(err.contains(key), "{key}: {err}");
            assert!(err.contains("Chief of Staff"), "{err}");
        }
    }

    #[test]
    fn an_unknown_field_is_refused_with_the_ones_that_work() {
        let err =
            parse(&roster(&format!("{CHIEF}- instructions: be nice\n"))).expect_err("refused");

        assert!(err.contains("instructions"), "{err}");
        for key in KEYS {
            assert!(err.contains(key), "{err}");
        }
    }

    #[test]
    fn a_field_given_twice_is_refused() {
        let err = parse(&roster(&format!("{CHIEF}- tools: shell_exec\n"))).expect_err("refused");
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn two_identities_with_one_name_are_refused() {
        let err = parse(&roster(&format!(
            "{REVIEWER}{}",
            REVIEWER.replace("Reviewer", "reviewer")
        )))
        .expect_err("refused");
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn a_roster_that_names_nobody_is_refused() {
        let err = parse("# Roster\n\n## Open questions\n\n- who?\n").expect_err("refused");
        assert!(err.contains("no identity"), "{err}");
    }

    #[test]
    fn runs_per_day_is_a_whole_number() {
        let err = parse(&roster(
            &REVIEWER.replace("runs_per_day: 0", "runs_per_day: often"),
        ))
        .expect_err("refused");
        assert!(err.contains("often"), "{err}");
    }

    #[test]
    fn a_dash_line_that_is_not_a_field_is_refused() {
        let err =
            parse(&roster(&format!("{REVIEWER}- reads diffs carefully\n"))).expect_err("refused");
        assert!(err.contains("not a field"), "{err}");
    }

    /// Prose is for the reader. A sentence that happens to say "tools:" is not
    /// a grant, which is the whole reason fields carry a dash.
    #[test]
    fn prose_grants_nothing_and_none_is_an_empty_list() {
        let text = roster(&format!(
            "{REVIEWER}It should never get tools: shell_exec, fs_write.\n"
        ));
        let doc = parse(&text).expect("parses");

        assert_eq!(doc.identities[0].tools, [tool::FS_LIST, tool::FS_READ]);
        assert!(doc.identities[0].skills.is_empty());
    }

    #[test]
    fn intended_routines_and_open_questions_are_listed_and_are_not_identities() {
        let text = roster(&format!(
            "{REVIEWER}## Intended routines\n\n- Watch runs watch.digest daily, after one run \
             under watch\n\n## Open questions\n\n- which mailbox?\n- none\n"
        ));
        let doc = parse(&text).expect("parses");

        assert_eq!(doc.identities.len(), 1);
        assert_eq!(doc.intended_routines.len(), 1);
        assert_eq!(doc.open_questions, ["which mailbox?"]);
    }

    #[test]
    fn a_workspace_with_no_roster_has_no_proposal() {
        let f = Fixture::new();
        assert!(read(&f.root, &f.agents, &[], &[]).is_none());

        let err = apply(&f.root, &f.agents, &[], &[], "", &f.audit, "p1").expect_err("refused");
        assert!(err.to_string().contains(ROSTER_FILE), "{err}");
    }

    /// The exit, at the level of the module: the named identities with the
    /// named allow-lists, on the audit log as the operator's act, and nothing
    /// else — no routine, no connector, no world.
    #[test]
    fn apply_creates_the_named_identities_with_the_named_allow_lists_and_nothing_else() {
        let f = Fixture::new();
        f.propose(&roster(&format!(
            "{CHIEF}{REVIEWER}## Intended routines\n\n- Reviewer, weekly\n"
        )));

        let shown = f.shown(&[]);
        assert!(shown.appliable, "{shown:?}");
        assert!(shown
            .entries
            .iter()
            .all(|entry| entry.state == RosterEntryState::New));

        let (applied, lines) = f.apply_shown(&[]).expect("applied");
        assert_eq!(applied.created.len(), 2);
        assert!(applied.skipped.is_empty());

        let chief = f
            .agents
            .list()
            .into_iter()
            .find(|agent| agent.name == "Chief of Staff")
            .expect("created");
        assert_eq!(
            chief.tools,
            [
                tool::FS_LIST,
                tool::FS_READ,
                tool::FS_WRITE,
                tool::SKILL_RUN,
                tool::SKILL_RETURN,
                tool::HANDOFF_DELEGATE,
            ]
        );
        assert_eq!(chief.skills, [skills::COS_SKILL]);
        assert_eq!(chief.runs_per_day, 0);

        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert_eq!(line.tool, AGENT_CREATE);
            assert_eq!(line.decision, AuditDecision::Operator);
            assert!(line.session_id.is_empty(), "no session made it");
            assert!(
                line.args_redacted.contains(ROSTER_FILE),
                "{}",
                line.args_redacted
            );
        }

        assert!(!f.data.join("routines.json").exists(), "no routine");
        assert!(!f.data.join("connectors.json").exists(), "no connector");
        assert!(!f.root.join("world").exists(), "no world");
    }

    /// A Reviewer somebody made narrow stays narrow, whatever a roster in some
    /// other project proposes for the name.
    #[test]
    fn a_name_that_already_exists_is_skipped_and_never_widened() {
        let f = Fixture::new();
        let narrow = f.made("reviewer", &[tool::FS_READ]);
        f.propose(&roster(&format!(
            "{CHIEF}{}",
            REVIEWER.replace("fs_list, fs_read", "fs_list, fs_read, fs_write, shell_exec")
        )));

        let shown = f.shown(&[]);
        assert_eq!(shown.entries[1].state, RosterEntryState::Present);

        let (applied, _) = f.apply_shown(&[]).expect("applied");
        assert_eq!(applied.skipped, ["Reviewer"]);
        assert_eq!(f.agents.get(&narrow.id).expect("still there"), narrow);
    }

    #[test]
    fn the_builtin_assistant_is_never_a_target() {
        let f = Fixture::new();
        f.propose(&roster(&format!(
            "{CHIEF}{}",
            REVIEWER
                .replace("## Reviewer", "## Assistant")
                .replace("none", "cabinet.found")
        )));

        let shown = f.shown(&[]);
        assert_eq!(shown.entries[1].state, RosterEntryState::Builtin);

        let (applied, _) = f.apply_shown(&[]).expect("applied");
        assert_eq!(applied.skipped, ["Assistant"]);
        assert_eq!(f.agents.list()[0], Agent::builtin());
        assert!(f.agents.list()[0].skills.is_empty());
    }

    /// Confirming apply is signing what was on screen. A file rewritten after
    /// that is not what was signed.
    #[test]
    fn a_roster_changed_after_it_was_shown_is_not_applied() {
        let f = Fixture::new();
        f.propose(&roster(REVIEWER));
        let shown = f.shown(&[]);

        f.propose(&roster(
            &REVIEWER.replace("fs_list, fs_read", "fs_read, shell_exec"),
        ));
        let err = apply(&f.root, &f.agents, &[], &[], &shown.digest, &f.audit, "p1")
            .expect_err("refused");

        assert!(err.to_string().contains("changed"), "{err}");
        assert_eq!(f.agents.list().len(), 1, "nobody was created");
    }

    #[test]
    fn one_identity_that_would_be_refused_means_none_is_created() {
        let f = Fixture::new();
        f.propose(&roster(&format!(
            "{CHIEF}{}",
            REVIEWER.replace("fs_list, fs_read", "fs_read, net_fetch")
        )));

        let shown = f.shown(&[]);
        assert!(!shown.appliable);
        assert!(shown.entries[0].problem.is_none());
        let problem = shown.entries[1].problem.as_deref().unwrap_or_default();
        assert!(problem.contains("net_fetch"), "{problem}");

        let err = f.apply_shown(&[]).expect_err("refused");
        assert!(err.to_string().contains("Nothing was created"), "{err}");
        assert_eq!(f.agents.list().len(), 1);
        assert!(f.audit.tail(10, None).expect("tail").is_empty());
    }

    #[test]
    fn a_connector_tool_nothing_answers_to_is_refused_and_a_live_one_is_granted() {
        let f = Fixture::new();
        f.propose(&roster(
            &REVIEWER.replace("fs_list, fs_read", "fs_read, git__status"),
        ));

        let dead = f.shown(&[]);
        let problem = dead.entries[0].problem.as_deref().unwrap_or_default();
        assert!(problem.contains("git__status"), "{problem}");
        assert!(f.apply_shown(&[]).is_err());

        let live = ["git__status".to_owned()];
        let (applied, _) = f.apply_shown(&live).expect("applied");
        assert_eq!(applied.created[0].tools, [tool::FS_READ, "git__status"]);
    }

    /// Allowed, the way the form allows it, and said out loud.
    #[test]
    fn a_runbook_the_identity_could_not_run_is_noted_and_does_not_block() {
        let dir = TempDir::new().expect("temp dir");
        let library = dir.path().join(skills::LIBRARY_DIR);
        skills::seed(&library);
        let catalog = skills::catalog(&library, None);

        let f = Fixture::new();
        f.propose(&roster(&REVIEWER.replace(
            "- tools: fs_list, fs_read\n- skills: none",
            "- tools: fs_list, fs_read, skill_run, skill_return\n- skills: review.diff, not.here",
        )));

        let shown = read(&f.root, &f.agents, &[], &catalog).expect("a proposal");
        assert!(shown.appliable, "{shown:?}");
        let notes = &shown.entries[0].notes;
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains(tool::SHELL_EXEC), "{notes:?}");
        assert!(notes[1].contains("not.here"), "{notes:?}");
    }

    #[test]
    fn a_roster_that_will_not_parse_is_listed_with_the_reason_and_never_applied() {
        let f = Fixture::new();
        f.propose("# Roster\n\nNobody yet.\n");

        let shown = f.shown(&[]);
        assert!(shown.problem.is_some());
        assert!(!shown.appliable);
        assert!(
            !shown.digest.is_empty(),
            "still signed, so a fix is a new digest"
        );

        let err = f.apply_shown(&[]).expect_err("refused");
        assert!(err.to_string().contains("never applied"), "{err}");
    }
}
