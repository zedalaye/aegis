//! The connector document: `connectors.json` (PLAN 7.3, Phase 18).
//!
//! A connector is an external MCP server: a program on this machine that Aegis
//! starts, speaks JSON-RPC to over its own stdin and stdout, and asks for a
//! list of tools. This module is the *record* of one — the id, the program, its
//! arguments, the environment it is given — the way [`routines`](super::routines)
//! is the record of a clock. What talks to it is [`mcp`](crate::mcp), and what
//! the tools it exposes are allowed to do is the same decision table everything
//! else goes through.
//!
//! Four decisions shape it, and each one is a thing this file makes impossible
//! rather than a thing it warns about.
//!
//! **Only a person adds a connector.** There is no `connector_add` tool and
//! there never will be: adding one names a program to run, and a model that
//! could name a program to run would have `shell_exec` without the dialog in
//! front of it. The whole surface is Settings, which is a human typing.
//!
//! **The id is part of the tool's name.** A connector's tools reach the model
//! as `<id>__<tool>` — `git__status`, `files__read_text_file` — so the id is
//! not a label, it is a namespace. That is why it is restricted to lower-case
//! letters, digits and `-`, with no `_` at all: the split at the first `__` is
//! then unambiguous, and no connector can be named so as to collide with a
//! built-in tool (`fs_read`, `shell_exec`) or to shadow another connector.
//!
//! **Secrets are not in this file.** A connector says which environment
//! variables it needs by *name*; the values come from the environment Aegis
//! itself was started in. Nothing here writes a token to disk, and there is no
//! field to put one in. The child is given exactly the variables the connector
//! names plus the platform's minimum ([`mcp::child_env`](crate::mcp::child_env))
//! — not the whole environment this process happens to hold, which is the one
//! place a connector is handled more carefully than `shell_exec` handles a
//! command.
//!
//! **Whether it is running is not stored.** A connector that was connected
//! when the process died is not connected now. That state lives in
//! [`mcp::Connectors`](crate::mcp::Connectors) and is measured, never
//! persisted — the property every document in [`store`](super) is held to.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};

/// Name of the document under the application-data directory.
const CONNECTORS_FILE: &str = "connectors.json";

/// Schema version of [`ConnectorsFile`].
const SCHEMA_VERSION: u32 = 1;

/// Longest connector id.
///
/// Short on purpose: it is a prefix on every tool name the model reads, and
/// the providers cap a function name at 64 characters. A long id spends that
/// budget on the connector rather than on the tool.
pub const ID_MAX_CHARS: usize = 24;

/// Longest human label.
const NAME_MAX_CHARS: usize = 48;

/// Most arguments a connector may carry.
const ARGS_MAX: usize = 32;

/// Most environment variables a connector may name.
const ENV_MAX: usize = 16;

/// Most connectors one installation may hold.
///
/// Each one is a process Aegis keeps alive, and each one's tools are read by
/// the model on every request. The ceiling is about the prompt more than about
/// the processes: a roster nobody narrowed is the "one generalist agent with
/// every MCP connector loaded" PLAN 7.5 refuses.
pub const CONNECTORS_MAX: usize = 16;

/// The separator between a connector's id and one of its tool names.
///
/// Two underscores rather than a `.` or a `/`, because the providers accept
/// `[A-Za-z0-9_-]` in a function name and nothing else. An id may not contain
/// `_`, so the split at the first occurrence is the only split there is.
pub const NAME_SEPARATOR: &str = "__";

/// One external MCP server, as it is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Connector {
    /// Stable id, and the namespace its tools are offered under.
    pub id: String,
    /// How it is named to a person. Never reaches a tool name.
    pub name: String,
    /// The program to run. Looked up on PATH like any other.
    pub command: String,
    /// Its arguments, passed as a vector — never through a shell.
    pub args: Vec<String>,
    /// Environment variables the server needs, by name.
    ///
    /// The values are read from Aegis' own environment when the child is
    /// spawned. A name that is not set there is reported on the row rather
    /// than passed as an empty string, because a server that reads an empty
    /// token usually fails in a way that is much harder to read.
    pub env: Vec<String>,
    /// Whether Aegis starts it at all.
    pub enabled: bool,
}

/// A connector as a person typed it into the form.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ConnectorDraft {
    /// Stable id, and the namespace its tools are offered under.
    pub id: String,
    /// How it is named to a person.
    pub name: String,
    /// The program to run.
    pub command: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables it needs, by name.
    #[serde(default)]
    pub env: Vec<String>,
    /// Whether Aegis starts it.
    #[serde(default)]
    pub enabled: bool,
}

/// Whether `id` may name a connector.
///
/// Lower-case letters, digits and `-`, starting with a letter or a digit. No
/// `_`, which is what keeps `<id>__<tool>` splittable, and no `.`, which the
/// providers do not accept in a function name.
pub fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= ID_MAX_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && id
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The name a connector's tool is offered to the model under.
pub fn tool_name(connector: &str, tool: &str) -> String {
    format!("{connector}{NAME_SEPARATOR}{tool}")
}

/// Splits a tool name back into the connector and the tool, when it is one.
///
/// Deliberately ignorant of what is installed: this is a question about the
/// *shape* of a name, asked by [`ToolCall::parse`](crate::policy::ToolCall) in
/// a place that has no connector list to consult. Whether anything answers to
/// the id is settled later, by the thing that would have to make the call.
pub fn split_tool_name(name: &str) -> Option<(&str, &str)> {
    let (connector, tool) = name.split_once(NAME_SEPARATOR)?;
    if !is_id(connector) || tool.is_empty() {
        return None;
    }
    Some((connector, tool))
}

/// Wire shape of `connectors.json`.
#[derive(Debug, Serialize, Deserialize)]
struct ConnectorsFile {
    version: u32,
    connectors: Vec<Connector>,
}

/// The connector list, behind its own lock.
#[derive(Debug)]
pub struct ConnectorStore {
    path: PathBuf,
    connectors: Mutex<Vec<Connector>>,
}

impl ConnectorStore {
    /// Loads the store from `data_dir`.
    ///
    /// Never fails, for the reason the other documents do not: a tray app that
    /// will not boot cannot explain why it did not. A damaged document costs
    /// the connectors in it — nothing starts, and the panel is empty — rather
    /// than the app.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(CONNECTORS_FILE);

        let connectors = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ConnectorsFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(count = file.connectors.len(), "connector store loaded");
                    file.connectors
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown connector store version"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "connector store is not readable JSON");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                tracing::error!(%err, "connector store is not readable");
                Vec::new()
            }
        };

        Self {
            path,
            connectors: Mutex::new(connectors),
        }
    }

    /// Every connector, in the order they were added.
    pub fn list(&self) -> Vec<Connector> {
        self.read().clone()
    }

    /// One connector by id.
    pub fn get(&self, id: &str) -> AppResult<Connector> {
        self.read()
            .iter()
            .find(|connector| connector.id == id)
            .cloned()
            .ok_or_else(|| AppError::ConnectorNotFound { id: id.to_owned() })
    }

    /// Every connector Aegis should be running.
    pub fn enabled(&self) -> Vec<Connector> {
        self.read()
            .iter()
            .filter(|connector| connector.enabled)
            .cloned()
            .collect()
    }

    /// Creates a connector, or replaces one.
    ///
    /// `editing` is the id of the row being changed; `None` creates. The id is
    /// itself editable, which is why the uniqueness check takes it: renaming a
    /// connector renames every one of its tools, and the identities that were
    /// granted the old names do not hold the new ones. That is not a bug to
    /// paper over — an allow-list that silently followed a rename would be an
    /// allow-list that grants what nobody read.
    pub fn save(&self, editing: Option<&str>, draft: &ConnectorDraft) -> AppResult<Connector> {
        let mut connectors = self.write();

        if let Some(id) = editing {
            if !connectors.iter().any(|connector| connector.id == id) {
                return Err(AppError::ConnectorNotFound { id: id.to_owned() });
            }
        } else if connectors.len() >= CONNECTORS_MAX {
            return Err(AppError::Connector {
                field: "name",
                reason: format!(
                    "this build holds at most {CONNECTORS_MAX} connectors. Every one of them is \
                     a process, and a block of tool descriptions in every request — narrow the \
                     list rather than widening it"
                ),
            });
        }

        let checked = check(draft, &connectors, editing)?;

        match editing {
            Some(id) => {
                let at = connectors
                    .iter()
                    .position(|connector| connector.id == id)
                    .ok_or_else(|| AppError::ConnectorNotFound { id: id.to_owned() })?;
                connectors[at] = checked.clone();
            }
            None => connectors.push(checked.clone()),
        }

        self.persist(&connectors)?;
        Ok(checked)
    }

    /// Deletes a connector.
    ///
    /// The identities that were granted its tools keep those names in their
    /// allow-lists. They are refused anyway — nothing answers to them — and
    /// leaving them is the honest behaviour: re-adding the connector should not
    /// silently re-grant it, and rewriting every identity because one row was
    /// deleted is a change nobody asked for. Settings says which grants no
    /// longer point at anything.
    pub fn delete(&self, id: &str) -> AppResult<()> {
        let mut connectors = self.write();
        let before = connectors.len();
        connectors.retain(|connector| connector.id != id);
        if connectors.len() == before {
            return Err(AppError::ConnectorNotFound { id: id.to_owned() });
        }
        self.persist(&connectors)
    }

    /// Stops or restarts a connector.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> AppResult<Connector> {
        let mut connectors = self.write();
        let at = connectors
            .iter()
            .position(|connector| connector.id == id)
            .ok_or_else(|| AppError::ConnectorNotFound { id: id.to_owned() })?;
        connectors[at].enabled = enabled;
        let updated = connectors[at].clone();
        self.persist(&connectors)?;
        Ok(updated)
    }

    /// Where the document lives, for the Settings note.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes the document.
    fn persist(&self, connectors: &[Connector]) -> AppResult<()> {
        let file = ConnectorsFile {
            version: SCHEMA_VERSION,
            connectors: connectors.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| AppError::Store {
            action: "serialize",
            source: io::Error::other(err),
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(
                %err,
                path = %self.path.display(),
                "could not write the connector store"
            );
            AppError::Store {
                action: "save",
                source: err,
            }
        })
    }

    /// The list, for reading. Poisoning is recovered from rather than
    /// propagated, like every other store here.
    fn read(&self) -> MutexGuard<'_, Vec<Connector>> {
        self.connectors.lock().unwrap_or_else(|poisoned| {
            tracing::error!("the connector store lock was poisoned; recovering");
            poisoned.into_inner()
        })
    }

    /// The list, for writing.
    fn write(&self) -> MutexGuard<'_, Vec<Connector>> {
        self.read()
    }
}

/// Checks a draft and normalizes it.
fn check(
    draft: &ConnectorDraft,
    connectors: &[Connector],
    editing: Option<&str>,
) -> AppResult<Connector> {
    let id = draft.id.trim();
    if !is_id(id) {
        return Err(AppError::Connector {
            field: "id",
            reason: format!(
                "`{id}` cannot be an id. Use lower-case letters, digits and `-`, up to \
                 {ID_MAX_CHARS} characters, like `git` or `github-issues`. No underscore: the id \
                 is the part before the `__` in every tool name this connector offers, and one \
                 in the id would make that split ambiguous"
            ),
        });
    }
    if connectors
        .iter()
        .filter(|connector| Some(connector.id.as_str()) != editing)
        .any(|connector| connector.id == id)
    {
        return Err(AppError::Connector {
            field: "id",
            reason: format!(
                "`{id}` is already a connector. Two connectors under one id would offer the \
                 model two different tools under one name"
            ),
        });
    }
    // Checked against the tools this build declares itself. The separator makes
    // a collision impossible today; the check is what keeps it impossible if a
    // built-in is ever named with a `__` in it.
    if crate::tools::names()
        .iter()
        .any(|name| name.starts_with(id) && name[id.len()..].starts_with(NAME_SEPARATOR))
    {
        return Err(AppError::Connector {
            field: "id",
            reason: format!("`{id}` would shadow a tool this build already has"),
        });
    }

    let name = draft.name.trim();
    if name.is_empty() {
        return Err(AppError::Connector {
            field: "name",
            reason: "give it a name a person will recognize — \"GitHub\", \"Local files\""
                .to_owned(),
        });
    }
    if name.chars().count() > NAME_MAX_CHARS {
        return Err(AppError::Connector {
            field: "name",
            reason: format!("keep it under {NAME_MAX_CHARS} characters"),
        });
    }

    let command = draft.command.trim();
    if command.is_empty() {
        return Err(AppError::Connector {
            field: "command",
            reason: "name the program that speaks MCP on its stdin and stdout — `npx`, `uvx`, \
                     `docker`, or the path to a binary. There is no shell here: put each \
                     argument in the list below rather than writing one line"
                .to_owned(),
        });
    }
    // A connector is a program, not a command line. Refusing a shell here is
    // the same rule `shell_exec` follows for the same reason: an argument
    // vector is a thing a person can read, and `-c "..."` is not.
    if looks_like_a_shell(command) {
        return Err(AppError::Connector {
            field: "command",
            reason: format!(
                "`{command}` is a shell, and a connector is not a command line. Name the program \
                 itself and put its arguments in the list"
            ),
        });
    }

    if draft.args.len() > ARGS_MAX {
        return Err(AppError::Connector {
            field: "args",
            reason: format!("at most {ARGS_MAX} arguments"),
        });
    }

    let mut env: Vec<String> = Vec::new();
    for variable in &draft.env {
        let variable = variable.trim();
        if variable.is_empty() {
            continue;
        }
        if !variable
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(AppError::Connector {
                field: "env",
                reason: format!(
                    "`{variable}` is not the name of an environment variable. Name the variable \
                     — `GITHUB_TOKEN` — not its value: Aegis reads it from its own environment \
                     when it starts the connector, and never writes it to a file"
                ),
            });
        }
        if !env.iter().any(|kept| kept == variable) {
            env.push(variable.to_owned());
        }
    }
    if env.len() > ENV_MAX {
        return Err(AppError::Connector {
            field: "env",
            reason: format!("at most {ENV_MAX} environment variables"),
        });
    }

    Ok(Connector {
        id: id.to_owned(),
        name: name.to_owned(),
        command: command.to_owned(),
        args: draft
            .args
            .iter()
            .map(|argument| argument.trim().to_owned())
            .filter(|argument| !argument.is_empty())
            .collect(),
        env,
        enabled: draft.enabled,
    })
}

/// Whether `command` names a shell rather than a program.
fn looks_like_a_shell(command: &str) -> bool {
    let base = Path::new(command)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(command);
    matches!(
        base.to_ascii_lowercase().as_str(),
        "sh" | "bash" | "zsh" | "fish" | "cmd" | "powershell" | "pwsh"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(id: &str) -> ConnectorDraft {
        ConnectorDraft {
            id: id.to_owned(),
            name: "Local files".to_owned(),
            command: "npx".to_owned(),
            args: vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-filesystem".to_owned(),
            ],
            env: Vec::new(),
            enabled: true,
        }
    }

    #[test]
    fn an_id_with_an_underscore_is_refused_because_the_split_would_be_ambiguous() {
        assert!(!is_id("my_files"));
        assert!(is_id("my-files"));
        assert!(is_id("git"));
        assert!(!is_id("Git"));
        assert!(!is_id("-git"));
        assert!(!is_id(""));
    }

    #[test]
    fn a_tool_name_splits_back_into_the_connector_and_the_tool() {
        assert_eq!(split_tool_name("git__status"), Some(("git", "status")));
        // The tool's own name may carry the separator; the connector's cannot,
        // so the first split is the only one.
        assert_eq!(
            split_tool_name("files__read__text"),
            Some(("files", "read__text"))
        );
        assert_eq!(split_tool_name("fs_read"), None);
        assert_eq!(split_tool_name("git__"), None);
    }

    #[test]
    fn a_shell_is_not_a_connector_command() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = ConnectorStore::load(dir.path());
        let mut bad = draft("files");
        bad.command = "bash".to_owned();
        let err = store.save(None, &bad).expect_err("refused");
        assert!(err.to_string().contains("shell"), "{err}");
    }

    #[test]
    fn a_value_in_the_env_list_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = ConnectorStore::load(dir.path());
        let mut bad = draft("files");
        bad.env = vec!["GITHUB_TOKEN=ghp_secret".to_owned()];
        let err = store.save(None, &bad).expect_err("refused");
        assert!(err.to_string().contains("Name the variable"), "{err}");
    }

    #[test]
    fn saving_survives_a_reload() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = ConnectorStore::load(dir.path());
        store.save(None, &draft("files")).expect("saved");

        let again = ConnectorStore::load(dir.path());
        assert_eq!(again.list().len(), 1);
        assert_eq!(again.enabled().len(), 1);
        assert_eq!(again.get("files").expect("there").command, "npx");
    }
}
