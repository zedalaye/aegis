//! The MCP client: tools that live in other processes (PLAN 7.3, Phase 18).
//!
//! A **connector** is an external MCP server the operator installed. Its tools
//! go through the same registry, matrix, dialog and audit as built-ins
//! (PLAN 7.1): this module yields [`ToolInfo`] and runs
//! [`ResolvedCall`](crate::policy::ResolvedCall)s policy already judged.
//!
//! 1. **Every connector call asks**; a server's read-only annotation only
//!    changes the dialog text (PLAN 7.2 row 7).
//! 2. **Grants are per tool** (`git__status`); tools added later are offered to
//!    nobody.
//! 3. **Installing is a human act** in Settings; granting tools to an identity
//!    is a second one.
//!
//! No client capabilities are advertised (no `sampling`, `roots`,
//! `elicitation`), and resources and prompts are not read — see
//! `docs/guide/connectors.md`. [`client`] is one connection; this module is the
//! roster.

pub mod client;

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use ts_rs::TS;

use crate::store::connectors::{self, Connector};

pub use client::{Notice, RawTool, ServerInfo, CALL_TIMEOUT, PROTOCOL_VERSION};

/// Most bytes of a connector's answer per envelope, like `shell_exec`'s output
/// cap.
pub const CALL_MAX_BYTES: u64 = 64 * 1024;

/// Longest tool description carried from a server into the prompt.
const DESCRIPTION_MAX_CHARS: usize = 1024;

// ---------------------------------------------------------------------------
// The catalog
// ---------------------------------------------------------------------------

/// One connector tool, as the rest of the runtime sees it.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolInfo {
    /// The connector's id.
    pub connector: String,
    /// The connector's human name, for the dialog and the panel.
    pub connector_name: String,
    /// The tool's own name, as the server spells it.
    pub name: String,
    /// `<connector>__<tool>`: the name in allow-lists, grants and audit lines.
    pub full_name: String,
    /// What the server says the tool does. Reaches the prompt verbatim.
    pub description: String,
    /// Whether the server *claims* the tool only reads; display only.
    pub read_only_hint: bool,
    /// The JSON Schema for its arguments (not sent to the WebView).
    #[serde(skip)]
    #[ts(skip)]
    pub parameters: Value,
}

impl ToolInfo {
    /// The tool as one entry of an OpenAI-compatible `tools` array.
    ///
    /// Same shape as [`ToolSpec::to_schema`](crate::tools::ToolSpec::to_schema).
    pub fn to_schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.full_name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// Every connector tool that is callable right now.
///
/// A per-turn snapshot, so the tools shown and the tools judged match.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    tools: Vec<ToolInfo>,
}

impl Catalog {
    /// A catalog with nothing in it — a build with no connectors, and what a
    /// test that has no opinion about them gets.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Builds a catalog from tools that are already resolved.
    pub fn of(tools: Vec<ToolInfo>) -> Self {
        Self { tools }
    }

    /// Whether anything is connected.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Every tool, in connector order.
    pub fn tools(&self) -> &[ToolInfo] {
        &self.tools
    }

    /// One tool by the name the model uses.
    pub fn find(&self, full_name: &str) -> Option<&ToolInfo> {
        self.tools.iter().find(|tool| tool.full_name == full_name)
    }

    /// The schemas for the tools an identity holds, in catalog order.
    ///
    /// Matched on the full tool name; one tool grants none of its siblings.
    pub fn schemas_for(&self, allowed: &[String]) -> Vec<Value> {
        self.tools
            .iter()
            .filter(|tool| allowed.contains(&tool.full_name))
            .map(ToolInfo::to_schema)
            .collect()
    }

    /// Every callable name, for the identity editor and for validation.
    pub fn names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.full_name.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The roster
// ---------------------------------------------------------------------------

/// Where one connector is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "bindings.ts")]
pub enum State {
    /// Configured, not started — nobody enabled it.
    Off,
    /// The process is starting, or the handshake is in flight.
    Starting,
    /// Connected, and its tools are in the catalog.
    Ready,
    /// It would not start, or it stopped.
    Failed,
}

/// A connector and everything measured about it right now.
///
/// The record from [`ConnectorStore`](crate::store::ConnectorStore) plus live
/// measurements, never stored.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ConnectorView {
    /// The stored record.
    pub connector: Connector,
    /// Where it is right now.
    pub state: State,
    /// What it offers, when it is connected.
    pub tools: Vec<ToolInfo>,
    /// Why it is not connected, when it is not.
    pub error: Option<String>,
    /// The last lines the server wrote to stderr, where start failures appear.
    pub log: Vec<String>,
    /// What the server said about itself, when it got that far.
    pub server: Option<String>,
    /// The protocol version that was agreed.
    pub protocol: Option<String>,
    /// Variables it names that this process's environment lacks (measured).
    pub missing_env: Vec<String>,
}

/// One connector's live half.
struct Live {
    connector: Connector,
    state: State,
    client: Option<Arc<client::Client>>,
    tools: Vec<ToolInfo>,
    info: ServerInfo,
    error: Option<String>,
}

/// Where a change of state is announced.
///
/// A closure, like [`EventSink`](crate::agent::EventSink), so tests need no
/// window.
pub type Announce = Arc<dyn Fn(ConnectorView) + Send + Sync>;

/// Shared state behind [`Connectors`].
#[derive(Default)]
struct Inner {
    live: Mutex<HashMap<String, Live>>,
    /// Where a change of state is announced, once the application installs one.
    announce: Mutex<Option<Announce>>,
}

/// The roster of connectors: what is running, and what each one offers.
///
/// Cheap to clone (an `Arc`), so supervisor tasks can own a copy.
#[derive(Clone, Default)]
pub struct Connectors {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Connectors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connectors")
            .field("live", &self.inner.live().len())
            .finish_non_exhaustive()
    }
}

impl Inner {
    fn live(&self) -> MutexGuard<'_, HashMap<String, Live>> {
        self.live.lock().unwrap_or_else(|poisoned| {
            tracing::error!("the connector roster lock was poisoned; recovering");
            poisoned.into_inner()
        })
    }

    fn announce(&self) -> Option<Announce> {
        self.announce
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Connectors {
    /// An empty roster.
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared roster with nothing in it, for a caller that has no connectors.
    ///
    /// `&'static`, since [`Turn`](crate::agent::Turn) and
    /// [`ToolCtx`](crate::tools::ToolCtx) borrow it.
    pub fn none() -> &'static Self {
        static NONE: std::sync::OnceLock<Connectors> = std::sync::OnceLock::new();
        NONE.get_or_init(Connectors::new)
    }

    /// Installs the sink a change of state is announced on.
    ///
    /// Set once, at startup, by the application.
    pub fn announce_on(&self, sink: Announce) {
        *self
            .inner
            .announce
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
    }

    /// The tools callable right now, in connector order.
    pub fn catalog(&self) -> Catalog {
        let live = self.inner.live();
        let mut ids: Vec<&String> = live.keys().collect();
        ids.sort();

        let mut tools = Vec::new();
        for id in ids {
            if let Some(one) = live.get(id) {
                if one.state == State::Ready {
                    tools.extend(one.tools.iter().cloned());
                }
            }
        }
        Catalog::of(tools)
    }

    /// Every connector's row, for the Settings panel.
    ///
    /// One row per `configured` connector, started or not.
    pub fn views(&self, configured: &[Connector]) -> Vec<ConnectorView> {
        let live = self.inner.live();
        configured
            .iter()
            .map(|connector| match live.get(&connector.id) {
                Some(one) => view(connector, one),
                None => ConnectorView {
                    connector: connector.clone(),
                    state: State::Off,
                    tools: Vec::new(),
                    error: None,
                    log: Vec::new(),
                    server: None,
                    protocol: None,
                    missing_env: missing_env(connector),
                },
            })
            .collect()
    }

    /// One connector's row.
    pub fn view(&self, connector: &Connector) -> ConnectorView {
        self.views(std::slice::from_ref(connector))
            .pop()
            .unwrap_or_else(|| ConnectorView {
                connector: connector.clone(),
                state: State::Off,
                tools: Vec::new(),
                error: None,
                log: Vec::new(),
                server: None,
                protocol: None,
                missing_env: Vec::new(),
            })
    }

    /// Starts a connector, replacing any connection it already had.
    ///
    /// Returns its row; a failed start is a row saying why, never an `Err`.
    pub async fn connect(&self, connector: &Connector) -> ConnectorView {
        self.disconnect(&connector.id).await;

        self.put(Live {
            connector: connector.clone(),
            state: State::Starting,
            client: None,
            tools: Vec::new(),
            info: ServerInfo::default(),
            error: None,
        });
        self.publish(connector);

        let (notices, inbox) = mpsc::unbounded_channel();
        let started = client::Client::start(connector, notices).await;

        match started {
            Ok((client, info, raw)) => {
                let client = Arc::new(client);
                let tools = catalog_of(connector, &raw);
                tracing::info!(
                    connector = %connector.id,
                    tools = tools.len(),
                    server = %info.name,
                    "a connector is ready"
                );
                self.put(Live {
                    connector: connector.clone(),
                    state: State::Ready,
                    client: Some(Arc::clone(&client)),
                    tools,
                    info,
                    error: None,
                });
                // Weak, so the supervisor cannot keep the roster — and through
                // it the child process — alive after the application is gone.
                supervise(Arc::downgrade(&self.inner), connector.id.clone(), inbox);
            }
            Err(error) => {
                tracing::warn!(connector = %connector.id, %error, "a connector would not start");
                self.put(Live {
                    connector: connector.clone(),
                    state: State::Failed,
                    client: None,
                    tools: Vec::new(),
                    info: ServerInfo::default(),
                    error: Some(error),
                });
            }
        }

        self.publish(connector);
        self.view(connector)
    }

    /// Stops a connector, if it is running.
    pub async fn disconnect(&self, id: &str) {
        let existing = {
            let mut live = self.inner.live();
            live.remove(id)
        };
        if let Some(one) = existing {
            if let Some(client) = one.client {
                client.shutdown().await;
            }
        }
    }

    /// Starts every connector that is enabled, one after another.
    ///
    /// Sequential, so cold `npx` fetches do not pile up.
    pub async fn connect_all(&self, configured: &[Connector]) {
        for connector in configured {
            if connector.enabled {
                self.connect(connector).await;
            }
        }
    }

    /// Stops everything. Called when the application quits.
    pub async fn shutdown(&self) {
        let ids: Vec<String> = self.inner.live().keys().cloned().collect();
        for id in ids {
            self.disconnect(&id).await;
        }
    }

    /// Makes one call, and returns the server's `result` object.
    ///
    /// The error is written for the model, inside an ordinary envelope
    /// (PLAN 4.3).
    pub async fn call(
        &self,
        connector: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<Value, String> {
        let client = {
            let live = self.inner.live();
            match live.get(connector) {
                Some(one) if one.state == State::Ready => one.client.clone(),
                Some(one) => {
                    return Err(match &one.error {
                        Some(error) => {
                            format!("the `{connector}` connector is not running: {error}")
                        }
                        None => format!("the `{connector}` connector is not running"),
                    })
                }
                None => {
                    return Err(format!(
                        "there is no connector called `{connector}` on this machine. The user \
                         installs connectors in Settings; you cannot"
                    ))
                }
            }
        };

        let Some(client) = client else {
            return Err(format!("the `{connector}` connector is not running"));
        };
        client.call_tool(tool, arguments).await
    }

    /// Records one connector's live half.
    fn put(&self, one: Live) {
        self.inner.live().insert(one.connector.id.clone(), one);
    }

    /// Tells the application a row changed.
    fn publish(&self, connector: &Connector) {
        if let Some(sink) = self.inner.announce() {
            sink(self.view(connector));
        }
    }
}

/// Watches one connection for what the server says on its own initiative.
///
/// `tools/list_changed` re-reads the catalog; a closed pipe marks the row as
/// gone.
fn supervise(inner: Weak<Inner>, id: String, mut inbox: mpsc::UnboundedReceiver<Notice>) {
    tokio::spawn(async move {
        while let Some(notice) = inbox.recv().await {
            let Some(inner) = inner.upgrade() else {
                return;
            };

            // Cloned out from under the lock: re-listing is a round trip, and
            // holding the roster across it would park every other connector.
            let (client, connector) = {
                let live = inner.live();
                match live.get(&id) {
                    Some(one) => (one.client.clone(), one.connector.clone()),
                    None => return,
                }
            };

            match notice {
                Notice::ToolsChanged => {
                    let Some(client) = client else { continue };
                    match client.list_tools().await {
                        Ok(raw) => {
                            let tools = catalog_of(&connector, &raw);
                            tracing::info!(connector = %id, count = tools.len(), "a connector changed what it offers");
                            let mut live = inner.live();
                            if let Some(one) = live.get_mut(&id) {
                                one.tools = tools;
                            }
                        }
                        Err(err) => {
                            tracing::warn!(connector = %id, %err, "a connector would not re-list its tools");
                        }
                    }
                }
                Notice::Closed(why) => {
                    let mut live = inner.live();
                    if let Some(one) = live.get_mut(&id) {
                        one.state = State::Failed;
                        one.tools.clear();
                        // Kept rather than dropped, so the last lines of its
                        // stderr are still readable on the row.
                        one.error = Some(why);
                    }
                }
            }

            // Announced after the lock is released, because a sink that reached
            // back into the roster would deadlock.
            let updated = {
                let live = inner.live();
                live.get(&id).map(|one| view(&one.connector, one))
            };
            if let (Some(sink), Some(updated)) = (inner.announce(), updated) {
                sink(updated);
            }
        }
    });
}

/// One connector's row, from its record and its live half.
fn view(connector: &Connector, one: &Live) -> ConnectorView {
    ConnectorView {
        connector: connector.clone(),
        state: one.state,
        tools: one.tools.clone(),
        error: one.error.clone(),
        log: one
            .client
            .as_ref()
            .map(|client| client.log())
            .unwrap_or_default(),
        server: (!one.info.name.is_empty()).then(|| {
            if one.info.version.is_empty() {
                one.info.name.clone()
            } else {
                format!("{} {}", one.info.name, one.info.version)
            }
        }),
        protocol: (!one.info.protocol.is_empty()).then(|| one.info.protocol.clone()),
        missing_env: missing_env(connector),
    }
}

/// The tools a server declared, named the way the model will see them.
fn catalog_of(connector: &Connector, raw: &[RawTool]) -> Vec<ToolInfo> {
    raw.iter()
        .map(|tool| {
            let mut description = tool.description.clone();
            if description.chars().count() > DESCRIPTION_MAX_CHARS {
                description = description
                    .chars()
                    .take(DESCRIPTION_MAX_CHARS)
                    .collect::<String>();
                description.push('…');
            }
            if description.is_empty() {
                description = format!("A tool of the `{}` connector.", connector.name);
            }

            ToolInfo {
                connector: connector.id.clone(),
                connector_name: connector.name.clone(),
                name: tool.name.clone(),
                full_name: connectors::tool_name(&connector.id, &tool.name),
                description,
                read_only_hint: tool.read_only_hint,
                parameters: tool.input_schema.clone(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The environment a connector is given
// ---------------------------------------------------------------------------

/// Variables every child needs whatever it is.
///
/// The floor (`PATH`, `SystemRoot`, ...); nothing else passes unless named on
/// the record.
#[cfg(windows)]
const BASELINE_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
];

/// The same floor on Unix.
#[cfg(not(windows))]
const BASELINE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "SHELL",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// The environment one connector's process is given.
///
/// The platform floor plus exactly the variables named on the record, not the
/// inherited environment (a connector is long-lived). Unset names are omitted,
/// not passed empty.
pub fn child_env(connector: &Connector) -> Vec<(String, OsString)> {
    let mut env = Vec::new();
    for name in BASELINE_ENV.iter().copied() {
        if let Some(value) = std::env::var_os(name) {
            env.push((name.to_owned(), value));
        }
    }
    for name in &connector.env {
        if env.iter().any(|(kept, _)| kept == name) {
            continue;
        }
        if let Some(value) = std::env::var_os(name) {
            env.push((name.clone(), value));
        }
    }
    env
}

/// The variables a connector names that this process does not hold.
pub fn missing_env(connector: &Connector) -> Vec<String> {
    connector
        .env
        .iter()
        .filter(|name| std::env::var_os(name).is_none())
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Reading an answer
// ---------------------------------------------------------------------------

/// What a connector's answer amounts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The text the model reads.
    pub text: String,
    /// Whether the server said the call failed.
    ///
    /// `isError`: the tool's own failure, returned as a failed envelope.
    pub failed: bool,
    /// How much text there was before truncation.
    pub bytes: u64,
    /// Whether [`Answer::text`] is shorter than what came back.
    pub truncated: bool,
}

/// Renders a `tools/call` result into the text the model sees.
///
/// Non-text content (e.g. base64 images) is described, never inlined.
pub fn read_answer(result: &Value) -> Answer {
    let failed = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut parts: Vec<String> = Vec::new();
    if let Some(blocks) = result.get("content").and_then(Value::as_array) {
        for block in blocks {
            parts.push(render_block(block));
        }
    }

    // A server that answered only with structured output still answered. The
    // specification allows it, and a model reading `{}` learns nothing.
    if parts.iter().all(|part| part.trim().is_empty()) {
        if let Some(structured) = result.get("structuredContent") {
            if !structured.is_null() {
                parts = vec![serde_json::to_string_pretty(structured)
                    .unwrap_or_else(|_| structured.to_string())];
            }
        }
    }

    let full = parts.join("\n");
    let bytes = full.len() as u64;
    let (text, truncated) = if bytes > CALL_MAX_BYTES {
        // On a character boundary, so the envelope is still valid UTF-8.
        let mut cut = CALL_MAX_BYTES as usize;
        while cut > 0 && !full.is_char_boundary(cut) {
            cut -= 1;
        }
        (full[..cut].to_owned(), true)
    } else {
        (full, false)
    };

    Answer {
        text,
        failed,
        bytes,
        truncated,
    }
}

/// One content block, as text.
fn render_block(block: &Value) -> String {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        Some(kind @ ("image" | "audio")) => {
            let mime = block
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let bytes = block
                .get("data")
                .and_then(Value::as_str)
                .map_or(0, |data| data.len() * 3 / 4);
            format!("[{kind}, {mime}, about {bytes} bytes — not shown in this build]")
        }
        Some("resource_link") => {
            let uri = block.get("uri").and_then(Value::as_str).unwrap_or("");
            format!("[resource: {uri}]")
        }
        Some("resource") => match block.pointer("/resource/text").and_then(Value::as_str) {
            Some(text) => {
                let uri = block
                    .pointer("/resource/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                format!("[{uri}]\n{text}")
            }
            None => {
                let uri = block
                    .pointer("/resource/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                format!("[resource: {uri} — binary, not shown]")
            }
        },
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connector(id: &str) -> Connector {
        Connector {
            id: id.to_owned(),
            name: "Local files".to_owned(),
            command: "node".to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            enabled: true,
        }
    }

    #[test]
    fn tools_are_named_for_the_connector_they_came_from() {
        let raw = vec![RawTool {
            name: "read_text_file".to_owned(),
            description: "Reads a file.".to_owned(),
            input_schema: json!({ "type": "object" }),
            read_only_hint: true,
        }];
        let tools = catalog_of(&connector("files"), &raw);
        assert_eq!(tools[0].full_name, "files__read_text_file");
        assert_eq!(tools[0].name, "read_text_file");
        assert_eq!(tools[0].connector, "files");
    }

    /// A server with nothing to say about a tool still has to say something:
    /// a model given a nameless function guesses.
    #[test]
    fn a_tool_with_no_description_is_still_described() {
        let raw = vec![RawTool {
            name: "ping".to_owned(),
            description: String::new(),
            input_schema: json!({}),
            read_only_hint: false,
        }];
        let tools = catalog_of(&connector("files"), &raw);
        assert!(tools[0].description.contains("Local files"));
    }

    /// A server cannot spend the context window on its own prose.
    #[test]
    fn a_long_description_is_bounded() {
        let raw = vec![RawTool {
            name: "ping".to_owned(),
            description: "x".repeat(DESCRIPTION_MAX_CHARS * 4),
            input_schema: json!({}),
            read_only_hint: false,
        }];
        let tools = catalog_of(&connector("files"), &raw);
        assert_eq!(
            tools[0].description.chars().count(),
            DESCRIPTION_MAX_CHARS + 1,
            "bounded, with an ellipsis"
        );
    }

    /// The allow-list is matched on the full name, so holding one tool of a
    /// connector holds none of the others.
    #[test]
    fn a_grant_of_one_tool_does_not_offer_the_others() {
        let raw = vec![
            RawTool {
                name: "status".to_owned(),
                description: "one".to_owned(),
                input_schema: json!({}),
                read_only_hint: true,
            },
            RawTool {
                name: "commit".to_owned(),
                description: "two".to_owned(),
                input_schema: json!({}),
                read_only_hint: false,
            },
        ];
        let catalog = Catalog::of(catalog_of(&connector("git"), &raw));
        let held = vec!["git__status".to_owned()];
        let schemas = catalog.schemas_for(&held);
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["function"]["name"], "git__status");
    }

    #[test]
    fn text_blocks_are_joined_and_a_failure_is_carried() {
        let answer = read_answer(&json!({
            "content": [
                { "type": "text", "text": "on branch main" },
                { "type": "text", "text": "nothing to commit" },
            ],
        }));
        assert_eq!(answer.text, "on branch main\nnothing to commit");
        assert!(!answer.failed);

        let failed = read_answer(&json!({
            "isError": true,
            "content": [{ "type": "text", "text": "not a repository" }],
        }));
        assert!(failed.failed);
        assert_eq!(failed.text, "not a repository");
    }

    /// An image is described rather than pasted: the base64 is worth nothing
    /// to the model through this path and would cost the whole window.
    #[test]
    fn an_image_is_described_not_inlined() {
        let answer = read_answer(&json!({
            "content": [{ "type": "image", "mimeType": "image/png", "data": "A".repeat(4000) }],
        }));
        assert!(
            answer.text.starts_with("[image, image/png"),
            "{}",
            answer.text
        );
        assert!(!answer.text.contains("AAAA"));
    }

    #[test]
    fn a_structured_only_answer_is_still_an_answer() {
        let answer = read_answer(&json!({
            "content": [],
            "structuredContent": { "ahead": 2 },
        }));
        assert!(answer.text.contains("\"ahead\""), "{}", answer.text);
    }

    #[test]
    fn a_long_answer_is_truncated_and_says_so() {
        let long = "x".repeat(CALL_MAX_BYTES as usize + 500);
        let answer = read_answer(&json!({ "content": [{ "type": "text", "text": long }] }));
        assert!(answer.truncated);
        assert_eq!(answer.bytes, CALL_MAX_BYTES + 500);
        assert_eq!(answer.text.len() as u64, CALL_MAX_BYTES);
    }

    /// The floor is there whatever the record says, and a variable nobody
    /// exported is left out rather than passed empty.
    #[test]
    fn the_child_environment_is_the_floor_plus_what_was_named() {
        let mut one = connector("files");
        one.env = vec!["AEGIS_TEST_ABSENT_VARIABLE".to_owned()];

        let env = child_env(&one);
        assert!(env.iter().any(|(name, _)| name == "PATH"));
        assert!(!env
            .iter()
            .any(|(name, _)| name == "AEGIS_TEST_ABSENT_VARIABLE"));
        assert_eq!(missing_env(&one), vec!["AEGIS_TEST_ABSENT_VARIABLE"]);
    }

    #[test]
    fn an_empty_catalog_offers_nothing() {
        let catalog = Catalog::empty();
        assert!(catalog.is_empty());
        assert!(catalog.schemas_for(&["git__status".to_owned()]).is_empty());
        assert!(catalog.find("git__status").is_none());
    }
}
