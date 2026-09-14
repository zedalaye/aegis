//! One connection to one MCP server (PLAN 7.3, Phase 18).
//!
//! Stdio only: newline-delimited JSON-RPC over the child's pipes; no HTTP
//! transport (PLAN 7.4).
//!
//! * **No client capabilities** in `initialize` — above all no `sampling`,
//!   which would let a server drive the model outside any session or gate.
//!   Such requests get "method not found".
//! * **Only the named environment** ([`child_env`](super::child_env)).
//! * **No decisions**: `tools/call` happens only after
//!   [`policy`](crate::policy) and [`tools::run`](crate::tools::run).

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

use crate::store::Connector;

/// `CREATE_NO_WINDOW` — the child gets no console.
///
/// As for `shell_exec`: no console window flashing at boot.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The protocol version this client speaks.
///
/// Sent in `initialize`; a different version in the answer is accepted.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// How long a handshake may take before the connector is called dead.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a single `tools/call` may take.
///
/// The same two minutes `shell_exec` allows a command, and for the same
/// reason: past that the honest answer is that it did not finish.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// How many lines of a server's stderr are kept for the Settings panel.
///
/// The last lines, where start failures are reported.
const LOG_LINES: usize = 40;

/// Longest single line accepted from a server's stdout.
///
/// Past this the connection is dropped rather than buffered.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// What a server told us about itself during the handshake.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    /// The server's own name, as it reported it.
    pub name: String,
    /// Its version, as it reported it.
    pub version: String,
    /// The protocol version it agreed to.
    pub protocol: String,
}

/// One tool as a server declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawTool {
    /// The tool's own name, without the connector's prefix.
    pub name: String,
    /// What the server says it does. Reaches the model's prompt verbatim.
    pub description: String,
    /// The JSON Schema for its arguments, as the server wrote it.
    pub input_schema: Value,
    /// Whether the server *claims* the tool only reads.
    ///
    /// Only changes the dialog text ([`policy::matrix`](crate::policy::matrix)).
    pub read_only_hint: bool,
}

/// Something a server said that was not an answer to a question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `notifications/tools/list_changed`: what it offers has changed.
    ToolsChanged,
    /// The process ended, however it ended.
    Closed(String),
}

/// A live connection to one server.
pub struct Client {
    inner: Arc<Inner>,
    /// The child, kept so it can be killed explicitly.
    child: AsyncMutex<Child>,
    /// The stdout reader and the stderr drain, aborted when this drops.
    tasks: Vec<JoinHandle<()>>,
}

/// The half of a connection the reader task shares.
struct Inner {
    /// The connector's id, for log lines.
    id: String,
    stdin: AsyncMutex<ChildStdin>,
    /// Requests waiting for an answer, by JSON-RPC id.
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    /// The last lines the server wrote to stderr.
    log: Mutex<VecDeque<String>>,
    /// False once the process has gone.
    alive: AtomicBool,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("id", &self.inner.id)
            .field("alive", &self.inner.alive.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The child is spawned with `kill_on_drop`, so dropping it is enough to
        // end the process; the tasks are aborted because a reader parked on a
        // pipe that will never close is a task that never returns.
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Client {
    /// Starts the program and completes the MCP handshake.
    ///
    /// Returns the connection, server info and tools. Errors carry the stderr
    /// tail for the Settings panel.
    pub async fn start(
        connector: &Connector,
        notices: mpsc::UnboundedSender<Notice>,
    ) -> Result<(Self, ServerInfo, Vec<RawTool>), String> {
        // `shell_exec`'s resolver, so `npx.cmd` launches on Windows; relative
        // names resolve against this process's directory.
        let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let program = crate::tools::shell::resolve(&connector.command, &here)?;

        let mut command = Command::new(&program);
        command
            .args(&connector.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        for (name, value) in super::child_env(connector) {
            command.env(name, value);
        }
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);

        let mut child = command.spawn().map_err(|err| {
            format!(
                "`{}` would not start: {err}. Check that the program is on this machine's PATH",
                connector.command
            )
        })?;

        // Taken rather than borrowed: the reader task outlives this function,
        // and the writer half lives inside the shared `Inner`.
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err("the connector's pipes could not be opened".to_owned());
        };

        let inner = Arc::new(Inner {
            id: connector.id.clone(),
            stdin: AsyncMutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            log: Mutex::new(VecDeque::new()),
            alive: AtomicBool::new(true),
        });

        let reader = tokio::spawn(read_stdout(
            Arc::clone(&inner),
            BufReader::new(stdout),
            notices,
        ));
        let draining = tokio::spawn(drain_stderr(Arc::clone(&inner), BufReader::new(stderr)));

        let client = Self {
            inner,
            child: AsyncMutex::new(child),
            tasks: vec![reader, draining],
        };

        // The handshake, in the order the specification fixes: `initialize`,
        // then the `initialized` notification, and only then may anything else
        // be asked. A server that answers `tools/list` before the notification
        // is within its rights to refuse.
        let hello = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    // Empty on purpose. See the module note: no `sampling`, no
                    // `roots`, no `elicitation`.
                    "capabilities": {},
                    "clientInfo": {
                        "name": "Aegis",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
                HANDSHAKE_TIMEOUT,
            )
            .await
            .map_err(|err| client.with_log(&err))?;

        client
            .notify("notifications/initialized", json!({}))
            .await
            .map_err(|err| client.with_log(&err))?;

        let info = ServerInfo {
            name: text_at(&hello, &["serverInfo", "name"]).unwrap_or_default(),
            version: text_at(&hello, &["serverInfo", "version"]).unwrap_or_default(),
            protocol: text_at(&hello, &["protocolVersion"]).unwrap_or_default(),
        };

        // A server that declares no `tools` capability has none to offer. It is
        // not an error — a resources-only server is a legitimate thing — but it
        // is a connector this build has nothing to do with, and saying so is
        // more useful than an empty list nobody can explain.
        if hello.pointer("/capabilities/tools").is_none() {
            return Err(format!(
                "`{}` connected but offers no tools — it declares no `tools` capability. This \
                 build uses connectors for their tools; resources and prompts are not read",
                connector.name
            ));
        }

        let tools = client
            .list_tools()
            .await
            .map_err(|err| client.with_log(&err))?;
        Ok((client, info, tools))
    }

    /// Asks the server what it offers.
    pub async fn list_tools(&self) -> Result<Vec<RawTool>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;

        // Paginated by the specification. Bounded here as well: a server that
        // kept handing back a cursor would otherwise be an infinite loop inside
        // a handshake.
        for _ in 0..16 {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self
                .request("tools/list", params, HANDSHAKE_TIMEOUT)
                .await?;

            let Some(listed) = page.get("tools").and_then(Value::as_array) else {
                return Err("the connector's tool list was not a list".to_owned());
            };
            for one in listed {
                if let Some(tool) = raw_tool(one) {
                    tools.push(tool);
                }
            }

            cursor = text_at(&page, &["nextCursor"]);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Ok(tools)
    }

    /// Calls one tool and returns the server's `result` object.
    ///
    /// Only the round trip: [`Connectors::call`](super::Connectors::call) builds
    /// the envelope and maps `isError` to a failed
    /// [`ToolResult`](crate::tools::ToolResult).
    pub async fn call_tool(&self, tool: &str, arguments: &Value) -> Result<Value, String> {
        self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
            CALL_TIMEOUT,
        )
        .await
    }

    /// Whether the process is still there.
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::Relaxed)
    }

    /// The last lines the server wrote to its stderr.
    pub fn log(&self) -> Vec<String> {
        self.inner.log().iter().cloned().collect()
    }

    /// Ends the process.
    pub async fn shutdown(&self) {
        self.inner.alive.store(false, Ordering::Relaxed);
        // No `shutdown` request: MCP over stdio ends by closing the input pipe,
        // and a server that ignores that is ended the way `shell_exec` ends a
        // command that overran.
        let mut child = self.child.lock().await;
        if let Err(err) = child.kill().await {
            tracing::debug!(%err, connector = %self.inner.id, "the connector was already gone");
        }
    }

    /// One request, and the answer to it.
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if !self.is_alive() {
            return Err("the connector is not running".to_owned());
        }

        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending().insert(id, tx);

        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        if let Err(err) = self.inner.write(&frame).await {
            self.inner.pending().remove(&id);
            return Err(err);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(answered)) => answered,
            // The reader task dropped the sender: the pipe closed under us.
            Ok(Err(_)) => Err("the connector stopped before it answered".to_owned()),
            Err(_) => {
                self.inner.pending().remove(&id);
                Err(format!(
                    "the connector did not answer `{method}` within {} seconds",
                    timeout.as_secs()
                ))
            }
        }
    }

    /// One notification. Nothing answers it, by definition.
    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.inner
            .write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    /// A failure with the server's own last words attached.
    ///
    /// The stderr tail usually holds the real diagnosis.
    fn with_log(&self, err: &str) -> String {
        let log = self.log();
        if log.is_empty() {
            return err.to_owned();
        }
        let tail: Vec<&str> = log.iter().rev().take(4).map(String::as_str).rev().collect();
        format!("{err} — it printed: {}", tail.join(" / "))
    }
}

impl Inner {
    /// Writes one frame, newline-terminated.
    async fn write(&self, frame: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(frame)
            .map_err(|err| format!("a request to the connector would not serialize: {err}"))?;
        line.push(b'\n');

        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&line)
            .await
            .map_err(|err| format!("the connector would not take the request: {err}"))?;
        stdin
            .flush()
            .await
            .map_err(|err| format!("the connector would not take the request: {err}"))
    }

    fn pending(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Result<Value, String>>>> {
        self.pending.lock().unwrap_or_else(|poisoned| {
            tracing::error!("a connector's request table was poisoned; recovering");
            poisoned.into_inner()
        })
    }

    fn log(&self) -> std::sync::MutexGuard<'_, VecDeque<String>> {
        self.log.lock().unwrap_or_else(|poisoned| {
            tracing::error!("a connector's log was poisoned; recovering");
            poisoned.into_inner()
        })
    }

    /// Fails every request still waiting. Called once, when the pipe closes.
    fn abandon(&self, why: &str) {
        let waiting: Vec<_> = self.pending().drain().collect();
        for (_, tx) in waiting {
            let _ = tx.send(Err(why.to_owned()));
        }
    }
}

/// Reads frames off the server's stdout until the pipe closes.
async fn read_stdout(
    inner: Arc<Inner>,
    stdout: BufReader<tokio::process::ChildStdout>,
    notices: mpsc::UnboundedSender<Notice>,
) {
    let mut lines = stdout.lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(%err, connector = %inner.id, "a connector's output could not be read");
                break;
            }
        };
        if line.len() > MAX_FRAME_BYTES {
            tracing::error!(connector = %inner.id, "a connector sent an oversized frame");
            break;
        }
        if line.trim().is_empty() {
            continue;
        }

        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            // Servers that print to stdout instead of stderr are common enough
            // that this is a log line, not a disconnection: the frames that do
            // parse still work.
            tracing::debug!(connector = %inner.id, "a connector wrote non-JSON to stdout");
            continue;
        };

        // An answer to something we asked.
        if let Some(id) = frame.get("id").and_then(Value::as_u64) {
            if frame.get("method").is_some() {
                // A *request* from the server. We advertised no capabilities,
                // so there is nothing it can legitimately ask for — and an
                // unanswered request would leave it waiting for ever.
                let method = text_at(&frame, &["method"]).unwrap_or_default();
                tracing::info!(connector = %inner.id, %method, "a connector asked for something this client does not offer");
                let refusal = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Aegis does not implement `{method}`"),
                    },
                });
                if let Err(err) = inner.write(&refusal).await {
                    tracing::debug!(%err, connector = %inner.id, "could not refuse a connector's request");
                }
                continue;
            }

            let Some(tx) = inner.pending().remove(&id) else {
                tracing::debug!(connector = %inner.id, id, "a connector answered a question nobody asked");
                continue;
            };
            let answer = match frame.get("error") {
                Some(error) => Err(rpc_error(error)),
                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(answer);
            continue;
        }

        // A notification.
        match frame.get("method").and_then(Value::as_str) {
            Some("notifications/tools/list_changed") => {
                let _ = notices.send(Notice::ToolsChanged);
            }
            Some("notifications/message") => {
                tracing::debug!(connector = %inner.id, "connector log: {}", frame);
            }
            Some(other) => {
                tracing::debug!(connector = %inner.id, method = other, "an unhandled connector notification");
            }
            None => {}
        }
    }

    inner.alive.store(false, Ordering::Relaxed);
    inner.abandon("the connector stopped");
    let _ = notices.send(Notice::Closed(
        "the connector's process ended — reconnect it in Settings".to_owned(),
    ));
    tracing::info!(connector = %inner.id, "a connector's output closed");
}

/// Keeps the last lines of a server's stderr, for the Settings row.
async fn drain_stderr(inner: Arc<Inner>, stderr: BufReader<tokio::process::ChildStderr>) {
    let mut lines = stderr.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        tracing::debug!(connector = %inner.id, "connector stderr: {line}");
        let mut log = inner.log();
        if log.len() == LOG_LINES {
            log.pop_front();
        }
        log.push_back(line);
    }
}

/// One tool from the server's `tools/list`, when it is well enough formed.
fn raw_tool(one: &Value) -> Option<RawTool> {
    let name = one.get("name").and_then(Value::as_str)?.trim().to_owned();
    if name.is_empty() {
        return None;
    }
    // The tool's name becomes half of a function name the provider has to
    // accept, so a server that offers `list files!` offers nothing.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        tracing::warn!(%name, "a connector offered a tool whose name a provider would refuse");
        return None;
    }

    // `title` is the human label and `description` the sentence for the model.
    // Both are the server's text; neither is trusted for anything but reading.
    let description = one
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();

    let input_schema = one
        .get("inputSchema")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));

    Some(RawTool {
        name,
        description,
        input_schema,
        read_only_hint: one
            .pointer("/annotations/readOnlyHint")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// A JSON-RPC error object, as a sentence.
fn rpc_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the connector refused the request");
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => format!("{message} (JSON-RPC {code})"),
        None => message.to_owned(),
    }
}

/// A string at a path in a JSON object, when there is one.
fn text_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut at = value;
    for key in path {
        at = at.get(key)?;
    }
    at.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_whose_name_a_provider_would_refuse_is_dropped() {
        assert!(raw_tool(&json!({ "name": "read file" })).is_none());
        assert!(raw_tool(&json!({ "name": "" })).is_none());
        assert!(raw_tool(&json!({ "description": "no name" })).is_none());
        assert_eq!(
            raw_tool(&json!({ "name": "read_file" }))
                .expect("kept")
                .name,
            "read_file"
        );
    }

    #[test]
    fn a_tool_with_no_schema_still_has_one() {
        let tool = raw_tool(&json!({ "name": "ping" })).expect("kept");
        assert_eq!(tool.input_schema["type"], "object");
    }

    /// The hint is read, and it is read as a claim: it lands in a field named
    /// after what it is.
    #[test]
    fn the_read_only_hint_is_carried_as_a_hint() {
        let tool = raw_tool(&json!({
            "name": "status",
            "annotations": { "readOnlyHint": true },
        }))
        .expect("kept");
        assert!(tool.read_only_hint);
        assert!(
            !raw_tool(&json!({ "name": "status" }))
                .expect("kept")
                .read_only_hint
        );
    }

    #[test]
    fn an_rpc_error_reads_as_a_sentence() {
        let rendered = rpc_error(&json!({ "code": -32602, "message": "bad arguments" }));
        assert_eq!(rendered, "bad arguments (JSON-RPC -32602)");
    }
}
