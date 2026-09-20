//! The shell tool: `shell_exec`.
//!
//! There is no shell. The program is looked up and spawned with its arguments
//! as a vector — no `sh -c`, no `cmd /c` around a string — so pipes, globs and
//! `&&` do not exist and there is no metacharacter layer (PLAN 3.3, 5.1). The
//! dialog shows exactly what runs; nothing is sandboxed.
//!
//! * **It ends**: a deadline, capped at 120 s (PLAN 4.3).
//! * **It can be stopped**: the turn's cancellation is in the same `select!` as
//!   the pipes.
//! * **It cannot flood**: 48 KB of head and 16 KB of tail in the envelope, and
//!   progress frames coalesced every ~50 ms and capped (PLAN 5.4). The pipes are
//!   still drained so the child never blocks.
//! * **It is answered**: a non-zero exit is a result (`ok: true`,
//!   `meta.exit_code`); `ok: false` means it did not run or did not finish.
//!
//! On Windows, `PATHEXT` resolution finds `.cmd` shims, and `Command` (Rust
//! ≥ 1.77.2) launches them through `cmd.exe` with the arguments escaped for
//! `cmd` — which a hand-written `cmd /c` would not do. Children get
//! `CREATE_NO_WINDOW`.
//!
//! With a WSL execution host (PLAN 7.12) the command runs as
//! `wsl.exe -d <distro> --cd <dir> --exec <program> <args>`: still a vector,
//! with no login shell. A probe checks the directory first, because `wsl --cd`
//! silently starts in `/`, and nothing ever falls back to this computer.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use super::{Produced, ProgressSink, Stream, EXEC_MAX_BYTES};
use crate::error::ErrorCode;
use crate::exec_host::ExecTarget;
use crate::policy::matrix::shell_line;
use crate::policy::tool;

mod output;
mod program;

use output::*;
pub(crate) use program::resolve;

/// The longest deadline a caller may ask for, in milliseconds, and the default
/// (PLAN 4.1): the user can press Stop, and a shorter default kills builds.
pub const TIMEOUT_CEILING_MS: u64 = 120_000;

/// How much of the output is kept from the start (PLAN 4.3).
const HEAD_BYTES: usize = 48 * 1024;

/// How much is kept from the end (PLAN 4.3).
const TAIL_BYTES: usize = 16 * 1024;

/// How long a `tool:progress` frame stays open (PLAN 5.4), as for `turn:delta`.
const PROGRESS_FRAME: Duration = Duration::from_millis(50);

/// Most output the progress stream carries. Past it the envelope and the audit
/// line remain, and `truncated` on `tool:finished` tells the UI.
const PROGRESS_MAX_BYTES: u64 = EXEC_MAX_BYTES;

/// Text that sends a frame early, so fast output still arrives smoothly.
const FRAME_MAX_BYTES: usize = 8 * 1024;

/// How much is read from a pipe at a time.
const PIPE_CHUNK: usize = 8 * 1024;

/// Chunks the pumps may queue. A child printing faster blocks on its own
/// write, which bounds memory.
const PIPE_QUEUE: usize = 16;

/// `CREATE_NO_WINDOW`: no console for the child (PLAN 5.1). Shared with
/// [`exec_host`](crate::exec_host).
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long the execution-host probe may take (PLAN 7.12): long enough to
/// start a stopped distribution.
#[cfg(windows)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// JSON Schema for `shell_exec` arguments (PLAN 4.1).
pub fn exec_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "program": {
                "type": "string",
                "description": "The program to run: a bare name looked up on PATH (`git`, \
                                `cargo`), or a path. Not a command line — it is never parsed by \
                                a shell.",
            },
            "args": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Arguments, one element each. `[\"commit\", \"-m\", \"a b\"]`, \
                                never `[\"commit -m 'a b'\"]`: nothing splits these for you, and \
                                nothing joins them either.",
            },
            "cwd": {
                "type": "string",
                "description": "Directory to run in. Relative paths are resolved against the \
                                workspace root, which is also the default.",
            },
            "timeout_ms": {
                "type": "integer",
                "minimum": 1,
                "maximum": TIMEOUT_CEILING_MS,
                "description": "Deadline in milliseconds. The command is killed when it passes. \
                                Capped at 120000, which is also the default.",
            },
        },
        "required": ["program"],
        "additionalProperties": false,
    })
}

/// Runs a program and collects its output.
///
/// `cwd` was judged by [`policy`](crate::policy); `program` is resolved here,
/// outside containment, since the approval is about the working directory.
/// With a `host` (PLAN 7.12) nothing is resolved here: PATH is the
/// distribution's.
pub(crate) async fn exec(
    program: &str,
    args: &[String],
    cwd: &Path,
    host: Option<&ExecTarget>,
    timeout_ms: Option<u64>,
    progress: &dyn ProgressSink,
    cancel: &CancellationToken,
) -> Produced {
    let line = shell_line(program, args);

    let launch = match plan(program, args, cwd, host, cancel).await {
        Ok(launch) => launch,
        Err(refused) => return *refused,
    };

    let budget = Duration::from_millis(
        timeout_ms
            .unwrap_or(TIMEOUT_CEILING_MS)
            .clamp(1, TIMEOUT_CEILING_MS),
    );

    let Launch {
        mut command,
        program: shown,
        cwd: directory,
        distro,
    } = launch;

    command
        // A command reading stdin would wait for input nobody can type.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The last defence against an orphan: if this future is dropped
        // without reaching the kill below, the child still goes.
        .kill_on_drop(true);

    // No console window per call (PLAN 5.1).
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!(%err, program = %shown, "a command would not start");
            return Produced::failed(
                tool::SHELL_EXEC,
                ErrorCode::ToolFailed,
                spawn_failure(Path::new(&shown), &err),
            );
        }
    };

    tracing::info!(program = %shown, cwd = %directory, host = %distro.as_deref().unwrap_or("this computer"), "running a command");

    let started = Instant::now();
    let run = Run {
        child,
        progress,
        cancel,
        budget,
    };
    let (ended, capture) = run.drive().await;

    // `as` saturates at `u64::MAX`, which is 584 million years.
    #[allow(clippy::cast_possible_truncation)]
    let duration_ms = started.elapsed().as_millis() as u64;
    let mut meta = json!({
        "program": shown,
        "cwd": directory,
        "duration_ms": duration_ms,
        "exit_code": ended.exit_code(),
        "bytes": capture.total,
    });
    // Only when the command landed elsewhere.
    if let Some(distro) = &distro {
        meta["exec_host"] = json!(distro);
    }

    match ended {
        Ended::Cancelled => Produced::cancelled(
            tool::SHELL_EXEC,
            format!(
                "`{line}` was stopped after {duration_ms} ms; it had produced {} of output",
                bytes_phrase(capture.total)
            ),
        ),
        Ended::TimedOut => Produced::failed(
            tool::SHELL_EXEC,
            ErrorCode::Timeout,
            format!(
                "`{line}` was killed after {} ms without finishing; it had produced {} of output",
                budget.as_millis(),
                bytes_phrase(capture.total)
            ),
        ),
        Ended::Exited(status) => {
            let (content, truncated) = capture.render();
            Produced::ok(
                tool::SHELL_EXEC,
                summarize(&line, status, duration_ms, capture.total, truncated),
                content,
                capture.total,
                truncated,
                meta,
            )
        }
        // The pipes closed but the process could not be reaped: no exit code
        // to report.
        Ended::Lost(message) => Produced::failed(
            tool::SHELL_EXEC,
            ErrorCode::ToolFailed,
            format!("`{line}` ran, but its result could not be read: {message}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// Where it lands
// ---------------------------------------------------------------------------

/// A configured child, and how the record names its program and directory —
/// built together, since the two hosts answer those differently.
struct Launch {
    /// The child, short of its pipes.
    command: Command,
    /// The program, as the record should name it: the resolved path on this
    /// computer, the name as given inside a distribution.
    program: String,
    /// The working directory, in the spelling of the host it runs on.
    cwd: String,
    /// The distribution, when the command lands in one.
    distro: Option<String>,
}

/// Decides where the command lands and prepares it. `Err` is a finished
/// envelope — `E_TOOL_FAILED`, `E_EXEC_HOST` or cancelled — boxed because
/// envelopes are wide.
async fn plan(
    program: &str,
    args: &[String],
    cwd: &Path,
    host: Option<&ExecTarget>,
    cancel: &CancellationToken,
) -> Result<Launch, Box<Produced>> {
    let Some(target) = host else {
        let resolved = resolve(program, cwd).map_err(|message| {
            Box::new(Produced::failed(
                tool::SHELL_EXEC,
                ErrorCode::ToolFailed,
                message,
            ))
        })?;

        let mut command = Command::new(&resolved);
        command.args(args).current_dir(cwd);

        return Ok(Launch {
            command,
            program: resolved.display().to_string(),
            cwd: cwd.display().to_string(),
            distro: None,
        });
    };

    wsl(program, args, target, cancel).await
}

/// Prepares a command inside a WSL distribution (PLAN 7.12), after the probe.
/// `--exec` keeps the vector with no login shell; `--cd` is the directory the
/// dialog showed.
#[cfg(windows)]
async fn wsl(
    program: &str,
    args: &[String],
    target: &ExecTarget,
    cancel: &CancellationToken,
) -> Result<Launch, Box<Produced>> {
    let Some(wsl) = crate::exec_host::wsl_exe() else {
        return Err(Box::new(Produced::failed(
            tool::SHELL_EXEC,
            ErrorCode::ExecHost,
            "this project runs its commands in a WSL distribution, and `wsl.exe` is not on this \
             machine"
                .to_owned(),
        )));
    };

    probe(&wsl, target, cancel).await?;

    let mut command = Command::new(&wsl);
    command
        .args(["-d", &target.distro, "--cd", &target.cwd, "--exec", program])
        .args(args);
    // No `current_dir`: `--cd` is the one answer to where it starts.

    Ok(Launch {
        command,
        program: program.to_owned(),
        cwd: target.cwd.clone(),
        distro: Some(target.distro.clone()),
    })
}

/// Off Windows a WSL host always refuses. Reachable through a `projects.json`
/// written on Windows; running here instead would be the wrong OS.
#[cfg(not(windows))]
async fn wsl(
    _program: &str,
    _args: &[String],
    target: &ExecTarget,
    _cancel: &CancellationToken,
) -> Result<Launch, Box<Produced>> {
    Err(Box::new(Produced::failed(
        tool::SHELL_EXEC,
        ErrorCode::ExecHost,
        format!(
            "this project runs its commands in the `{}` WSL distribution, and WSL is a Windows \
             feature. Clear the execution host to run them on this computer instead",
            target.distro
        ),
    )))
}

/// Asks the distribution whether it exists and can see the directory:
/// `test -d` exits 0 or 1, and anything else is WSL refusing. Worth a spawn per
/// call, because `wsl --cd` silently starts in `/` for a missing directory.
#[cfg(windows)]
async fn probe(
    wsl: &Path,
    target: &ExecTarget,
    cancel: &CancellationToken,
) -> Result<(), Box<Produced>> {
    let refused = |message: String| {
        Box::new(Produced::failed(
            tool::SHELL_EXEC,
            ErrorCode::ExecHost,
            message,
        ))
    };

    let mut command = Command::new(wsl);
    command
        .args(["-d", &target.distro, "--exec", "test", "-d", &target.cwd])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .creation_flags(CREATE_NO_WINDOW);

    let asked = tokio::select! {
        biased;
        () = cancel.cancelled() => {
            return Err(Box::new(Produced::cancelled(
                tool::SHELL_EXEC,
                "it was stopped before the distribution had answered".to_owned(),
            )));
        }
        () = tokio::time::sleep(PROBE_TIMEOUT) => {
            return Err(refused(format!(
                "`{}` did not answer within {} seconds, so nothing was run",
                target.distro,
                PROBE_TIMEOUT.as_secs()
            )));
        }
        asked = command.output() => asked,
    };

    let asked = asked.map_err(|err| {
        refused(format!(
            "`wsl.exe` would not start, so `{}` could not be reached: {err}",
            target.distro
        ))
    })?;

    match asked.status.code() {
        Some(0) => Ok(()),
        // `test` said no. The distribution is there; the folder is not.
        Some(1) => Err(refused(format!(
            "`{}` is not a folder inside the `{}` distribution, so the command was not run. The \
             workspace may be on a drive that distribution does not mount",
            target.cwd, target.distro
        ))),
        // WSL's own refusal, in its words, which say why.
        _ => {
            let said = crate::exec_host::message(&asked.stderr);
            let said = if said.is_empty() {
                String::new()
            } else {
                format!(": {said}")
            };
            Err(refused(format!(
                "`{}` is not a distribution a command can be run in right now{said}",
                target.distro
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

/// How a command stopped being this turn's problem.
enum Ended {
    /// It exited on its own.
    Exited(ExitStatus),
    /// It passed its deadline and was killed.
    TimedOut,
    /// The turn was cancelled and it was killed.
    Cancelled,
    /// It was neither reapable nor killable.
    Lost(String),
}

impl Ended {
    /// The exit code for `meta`. `None` when killed or signalled — never an
    /// invented `0`.
    fn exit_code(&self) -> Option<i32> {
        match self {
            Self::Exited(status) => status.code(),
            _ => None,
        }
    }
}

/// One running child, and everything that watches it.
struct Run<'a> {
    child: Child,
    progress: &'a dyn ProgressSink,
    cancel: &'a CancellationToken,
    budget: Duration,
}

impl Run<'_> {
    /// Drains both pipes — interleaved in arrival order, each chunk tagged with
    /// its pipe — until EOF or a deadline, then reaps the child.
    async fn drive(mut self) -> (Ended, Capture) {
        let (tx, mut rx) = mpsc::channel::<(Stream, Vec<u8>)>(PIPE_QUEUE);
        if let Some(pipe) = self.child.stdout.take() {
            tokio::spawn(pump(pipe, Stream::Stdout, tx.clone()));
        }
        if let Some(pipe) = self.child.stderr.take() {
            tokio::spawn(pump(pipe, Stream::Stderr, tx.clone()));
        }
        // Both senders now live in the tasks. Without this the channel never
        // closes and the loop below waits for a pipe nobody holds.
        drop(tx);

        let mut capture = Capture::default();
        let mut frames = Frames::default();

        let deadline = tokio::time::sleep(self.budget);
        tokio::pin!(deadline);

        let mut ticker = tokio::time::interval(PROGRESS_FRAME);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // `biased`: a Stop that lands alongside a chunk wins.
        let drained = loop {
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => break Some(Ended::Cancelled),
                () = &mut deadline => break Some(Ended::TimedOut),
                _ = ticker.tick() => frames.flush(self.progress),
                chunk = rx.recv() => match chunk {
                    Some((stream, bytes)) => {
                        capture.push(&bytes);
                        frames.push(stream, &bytes);
                        if frames.pending >= FRAME_MAX_BYTES {
                            frames.flush(self.progress);
                        }
                    }
                    None => break None,
                },
            }
        };

        // Both pipes at EOF usually means the child exited, but it may have
        // closed them and kept working, so the wait stays under both deadlines.
        let ended = match drained {
            Some(ended) => ended,
            None => tokio::select! {
                biased;
                () = self.cancel.cancelled() => Ended::Cancelled,
                () = &mut deadline => Ended::TimedOut,
                reaped = self.child.wait() => match reaped {
                    Ok(status) => Ended::Exited(status),
                    Err(err) => Ended::Lost(err.to_string()),
                },
            },
        };

        // Whatever the ending, the child does not outlive the call.
        if !matches!(ended, Ended::Exited(_)) {
            self.terminate().await;
        }

        frames.finish(self.progress);
        (ended, capture)
    }

    /// Kills a command that ran out of deadline or of turn.
    ///
    /// On Windows the whole tree (`taskkill /T`): a `.cmd` shim's `cmd.exe` is
    /// not the process doing the work, and Windows does not kill children with
    /// their parent. Ending `wsl.exe` also ends the Linux process it started
    /// (PLAN 7.12). On Unix only the child is killed: a script that spawns and
    /// waits can leave work running, until process groups are used.
    async fn terminate(&mut self) {
        #[cfg(windows)]
        if let Some(pid) = self.child.id() {
            let reaped = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .await;

            // Non-zero means the tree was already gone; `kill` below runs anyway.
            if let Err(err) = reaped {
                tracing::warn!(%err, "taskkill would not run");
            }
        }

        // `kill` waits for the process to actually go, so this cannot leave a
        // zombie behind either.
        if let Err(err) = self.child.kill().await {
            tracing::warn!(%err, "a command could not be killed");
        }
    }
}

/// Reads one pipe to EOF. A read error ends the pump: that is what a killed
/// child looks like.
async fn pump<R>(mut pipe: R, stream: Stream, tx: mpsc::Sender<(Stream, Vec<u8>)>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut buffer = vec![0u8; PIPE_CHUNK];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if tx.send((stream, buffer[..read].to_vec())).await.is_err() {
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

/// Why a resolved program still would not start.
fn spawn_failure(program: &Path, err: &std::io::Error) -> String {
    let shown = program.display();
    match err.kind() {
        std::io::ErrorKind::PermissionDenied => format!("`{shown}` is not executable by you"),
        // A batch file whose arguments cannot be escaped safely for `cmd.exe`.
        std::io::ErrorKind::InvalidInput => format!(
            "`{shown}` could not be started with those arguments: one of them cannot be passed \
             safely to a Windows batch file"
        ),
        std::io::ErrorKind::NotFound => format!("`{shown}` disappeared before it could be run"),
        _ => format!("`{shown}` could not be started: {err}"),
    }
}

/// The one-line result for the transcript and the audit log.
fn summarize(
    line: &str,
    status: ExitStatus,
    duration_ms: u64,
    bytes: u64,
    truncated: bool,
) -> String {
    let ending = match status.code() {
        Some(0) => "finished".to_owned(),
        Some(code) => format!("exited {code}"),
        None => terminated(status),
    };

    format!(
        "`{line}` {ending} in {duration_ms} ms, {}{}",
        bytes_phrase(bytes),
        if truncated { " (truncated)" } else { "" }
    )
}

/// How a command that produced no exit code ended.
#[cfg(unix)]
fn terminated(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt as _;

    status.signal().map_or_else(
        || "stopped".to_owned(),
        |signal| format!("killed by signal {signal}"),
    )
}

/// How a command that produced no exit code ended.
#[cfg(not(unix))]
fn terminated(_status: ExitStatus) -> String {
    "stopped".to_owned()
}

/// `no output`, `912 bytes of output`.
fn bytes_phrase(bytes: u64) -> String {
    match bytes {
        0 => "no output".to_owned(),
        1 => "1 byte of output".to_owned(),
        _ => format!("{bytes} bytes of output"),
    }
}

#[cfg(test)]
mod tests;
