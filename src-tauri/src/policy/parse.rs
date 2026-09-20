//! Reading a model's tool call into a [`ToolCall`]: the wire shapes of PLAN 4.1,
//! and the briefs and reports inside them.

use serde::Deserialize;

use super::*;

impl ToolCall {
    /// Parses the arguments a model produced. Errors are written for the model,
    /// which gets them back and can correct itself (PLAN 4.1). Unknown fields
    /// are ignored.
    pub fn parse(tool_name: &str, args: serde_json::Value) -> Result<Self, String> {
        fn convert<T: for<'de> Deserialize<'de>>(
            tool_name: &str,
            args: serde_json::Value,
        ) -> Result<T, String> {
            serde_json::from_value(args).map_err(|err| format!("{tool_name}: {err}"))
        }

        match tool_name {
            tool::FS_LIST => {
                let a: FsListArgs = convert(tool_name, args)?;
                Ok(Self::FsList {
                    path: a.path,
                    max_entries: a.max_entries,
                })
            }
            tool::FS_READ => {
                let a: FsReadArgs = convert(tool_name, args)?;
                Ok(Self::FsRead {
                    path: a.path,
                    offset: a.offset,
                    limit: a.limit,
                })
            }
            tool::FS_WRITE => {
                let a: FsWriteArgs = convert(tool_name, args)?;
                Ok(Self::FsWrite {
                    path: a.path,
                    content: a.content,
                    create_dirs: a.create_dirs,
                })
            }
            tool::SHELL_EXEC => {
                let a: ShellExecArgs = convert(tool_name, args)?;
                Ok(Self::ShellExec {
                    program: a.program,
                    args: a.args,
                    cwd: a.cwd,
                    timeout_ms: a.timeout_ms,
                })
            }
            tool::SCREEN_CAPTURE => {
                let a: ScreenCaptureArgs = convert(tool_name, args)?;
                Ok(Self::ScreenCapture { display: a.display })
            }
            tool::SKILL_RUN => {
                let a: SkillRunArgs = convert(tool_name, args)?;
                let name = a.name.trim();
                // Checked here because the allow-list matches this string next,
                // and a name like `../../etc` must never reach it.
                if !crate::skills::is_name(name) {
                    return Err(format!(
                        "`{name}` is not a skill name. They look like `inbox.triage`: lower-case \
                         letters, digits, `.`, `-` and `_`"
                    ));
                }
                Ok(Self::SkillRun {
                    name: name.to_owned(),
                })
            }
            tool::SKILL_RETURN => Ok(Self::SkillReturn {
                report: Box::new(draft(convert(tool_name, args)?)?),
            }),
            tool::HANDOFF_RETURN => Ok(Self::HandoffReturn {
                report: Box::new(draft(convert(tool_name, args)?)?),
            }),
            tool::HANDOFF_DELEGATE => {
                let a: HandoffDelegateArgs = convert(tool_name, args)?;
                let mut briefs = Vec::with_capacity(a.briefs.len());
                for one in a.briefs {
                    briefs.push(brief(one)?);
                }
                let review = match a.review {
                    Some(one) => Some(brief(one)?),
                    None => None,
                };
                Ok(Self::HandoffDelegate {
                    plan: Box::new(handoff::Plan { briefs, review }),
                })
            }
            tool::MEMORY_WRITE => {
                let a: MemoryWriteArgs = convert(tool_name, args)?;
                // Parsed here so the approval dialog can name the kind.
                let Some(kind) = memories::MemoryKind::parse(&a.kind) else {
                    return Err(format!(
                        "`{}` is not a kind of memory. A memory is a `preference`, an \
                         `exception` or a `convention`; anything else is a file in the workspace \
                         or a skill",
                        a.kind.trim()
                    ));
                };
                Ok(Self::MemoryWrite {
                    kind,
                    text: a.text,
                    source: a.source,
                })
            }
            tool::MEMORY_SEARCH => {
                let a: MemorySearchArgs = convert(tool_name, args)?;
                Ok(Self::MemorySearch {
                    query: a.query.unwrap_or_default(),
                })
            }
            // Before the connector arm, which would otherwise read these names.
            tool::JEV_EVAL => {
                let a: JevEvalArgs = convert(tool_name, args)?;
                let name = a.name.trim();
                if !crate::skills::is_name(name) {
                    return Err(format!(
                        "`{name}` is not an eval name. They look like `inbox.classify`"
                    ));
                }
                Ok(Self::JevEval {
                    name: name.to_owned(),
                    inputs: a.inputs,
                })
            }
            tool::JEV_ASK => {
                let a: JevAskArgs = convert(tool_name, args)?;
                decision::check_state(&a.state)?;
                Ok(Self::JevAsk {
                    state: a.state,
                    questions: decision::parse_questions(a.questions)?,
                })
            }
            // Shaped like a connector tool. Its arguments follow the server's
            // schema, so they are only checked to be an object (`tools/call`).
            other => match connectors::split_tool_name(other) {
                Some(_) if args.is_object() || args.is_null() => Ok(Self::Connector {
                    name: other.to_owned(),
                    args: if args.is_null() {
                        serde_json::json!({})
                    } else {
                        args
                    },
                }),
                Some(_) => Err(format!(
                    "`{other}` takes an object of arguments, and that was not one"
                )),
                None => Err(format!("unknown tool `{other}`")),
            },
        }
    }
}

/// Wire shape of `fs_list` arguments (PLAN 4.1).
#[derive(Debug, Deserialize)]
struct FsListArgs {
    path: String,
    #[serde(default)]
    max_entries: Option<u32>,
}

/// Wire shape of `fs_read` arguments.
#[derive(Debug, Deserialize)]
struct FsReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

/// Wire shape of `fs_write` arguments.
#[derive(Debug, Deserialize)]
struct FsWriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    create_dirs: bool,
}

/// Wire shape of `shell_exec` arguments.
#[derive(Debug, Deserialize)]
struct ShellExecArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// Wire shape of `screen_capture` arguments.
#[derive(Debug, Deserialize)]
struct ScreenCaptureArgs {
    #[serde(default)]
    display: Option<String>,
}

/// Wire shape of `skill_run` arguments.
#[derive(Debug, Deserialize)]
struct SkillRunArgs {
    name: String,
}

/// Wire shape of `memory_write` arguments. `kind` is a string so an unknown one
/// is answered with the three that work.
#[derive(Debug, Deserialize)]
struct MemoryWriteArgs {
    kind: String,
    text: String,
    #[serde(default)]
    source: Option<String>,
}

/// Wire shape of `jev_eval` arguments.
#[derive(Debug, Deserialize)]
struct JevEvalArgs {
    name: String,
    #[serde(default)]
    inputs: BTreeMap<String, String>,
}

/// Wire shape of `jev_ask` arguments.
#[derive(Debug, Deserialize)]
struct JevAskArgs {
    #[serde(default)]
    state: serde_json::Value,
    questions: Vec<decision::QuestionDraft>,
}

/// Wire shape of `memory_search` arguments.
#[derive(Debug, Deserialize)]
struct MemorySearchArgs {
    #[serde(default)]
    query: Option<String>,
}

/// Wire shape of a return, shared by `skill_return` and `handoff_return`
/// (`COS.md` *Handoff*). `status` is a string so an unknown one is answered with
/// the three that work.
#[derive(Debug, Deserialize)]
struct ReportArgs {
    status: String,
    summary: String,
    #[serde(default)]
    artefacts: Vec<String>,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    open_questions: Vec<String>,
    #[serde(default)]
    next_owner: Option<String>,
}

/// Wire shape of `handoff_delegate` arguments (`COS.md` *Handoff*).
#[derive(Debug, Deserialize)]
struct HandoffDelegateArgs {
    briefs: Vec<BriefArgs>,
    #[serde(default)]
    review: Option<BriefArgs>,
}

/// Wire shape of one brief.
///
/// `priority` and `return_format` arrive as strings, for the reason a status
/// does: an unrecognized one is then answered with the words that work.
#[derive(Debug, Deserialize)]
struct BriefArgs {
    goal: String,
    owner: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    constraints: Vec<String>,
    definition_of_done: String,
    #[serde(default)]
    approval_needed: Option<String>,
    #[serde(default)]
    return_format: Option<String>,
}

/// One return, from its arguments. Shared by both return tools.
fn draft(a: ReportArgs) -> Result<handoff::Draft, String> {
    let status = match a.status.trim() {
        "done" => handoff::Status::Done,
        "blocked" => handoff::Status::Blocked,
        "needs_you" => handoff::Status::NeedsYou,
        other => {
            return Err(format!(
                "`{other}` is not a status. A run ends `done`, `blocked` or `needs_you`"
            ))
        }
    };

    Ok(handoff::Draft {
        status,
        summary: a.summary,
        artefacts: a.artefacts,
        evidence: a.evidence,
        open_questions: a.open_questions,
        next_owner: a.next_owner.unwrap_or_default(),
    })
}

/// One brief, from its arguments. Only the words are checked here; the content
/// is [`handoff::check_brief`], so a refusal about content never looks like one
/// about spelling.
fn brief(a: BriefArgs) -> Result<handoff::Brief, String> {
    let priority = match a.priority.as_deref().map(str::trim) {
        None | Some("") | Some("normal") => handoff::Priority::Normal,
        Some("high") => handoff::Priority::High,
        Some("low") => handoff::Priority::Low,
        Some(other) => {
            return Err(format!(
                "`{other}` is not a priority. A brief is `high`, `normal` or `low`"
            ))
        }
    };

    let return_format = match a.return_format.as_deref().map(str::trim) {
        None | Some("") | Some("status") => handoff::ReturnFormat::Status,
        Some("artefact") => handoff::ReturnFormat::Artefact,
        Some("question") => handoff::ReturnFormat::Question,
        Some(other) => {
            return Err(format!(
                "`{other}` is not a return format. A brief asks for a `status`, an `artefact` or \
                 a `question`"
            ))
        }
    };

    Ok(handoff::Brief {
        goal: a.goal,
        owner: a.owner,
        priority,
        inputs: a.inputs,
        constraints: a.constraints,
        definition_of_done: a.definition_of_done,
        approval_needed: a.approval_needed.unwrap_or_default(),
        return_format,
    })
}
