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
// What the model sees
// ---------------------------------------------------------------------------

/// The bounded copy of a command's output (PLAN 4.3): head and tail, the useful
/// ends of a build log.
#[derive(Debug, Default)]
struct Capture {
    /// The first [`HEAD_BYTES`].
    head: Vec<u8>,
    /// The last [`TAIL_BYTES`] of everything after the head.
    tail: VecDeque<u8>,
    /// Everything the command produced, kept or not.
    total: u64,
}

impl Capture {
    /// Accounts for one chunk, keeping the ends and dropping the middle.
    fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);

        let room = HEAD_BYTES.saturating_sub(self.head.len());
        let take = room.min(bytes.len());
        self.head.extend_from_slice(&bytes[..take]);

        let rest = &bytes[take..];
        if rest.is_empty() {
            return;
        }

        // A chunk longer than the tail window replaces it outright.
        if rest.len() >= TAIL_BYTES {
            self.tail.clear();
            self.tail
                .extend(rest[rest.len() - TAIL_BYTES..].iter().copied());
            return;
        }

        self.tail.extend(rest.iter().copied());
        while self.tail.len() > TAIL_BYTES {
            self.tail.pop_front();
        }
    }

    /// The text for the envelope, and whether anything was dropped. Decoded,
    /// never refused, by the pane's [`Decoder`] spanning both halves; the
    /// elision marker is in the text so the model sees the gap.
    fn render(&self) -> (String, bool) {
        let kept = self.head.len().saturating_add(self.tail.len()) as u64;
        let elided = self.total.saturating_sub(kept);

        let mut decoder = Decoder::default();
        let mut sanitizer = Sanitizer::default();

        let mut text = sanitizer.push(&decoder.push(&self.head));
        text.push_str(&sanitizer.push(&decoder.finish()));

        if elided > 0 {
            // Inserted after the head is clean, so a control sequence the cut
            // interrupted cannot swallow the marker itself.
            sanitizer.discard();
            text.push_str(&format!("\n… {elided} bytes elided …\n"));
        }

        if !self.tail.is_empty() {
            let tail: Vec<u8> = self.tail.iter().copied().collect();
            // In UTF-8 the tail may start mid-character: drop those bytes. In a
            // legacy encoding the same bytes are letters.
            let start = if decoder.legacy {
                0
            } else {
                leading_fragment(&tail)
            };
            text.push_str(&sanitizer.push(&decoder.push(&tail[start..])));
            text.push_str(&sanitizer.push(&decoder.finish()));
        }
        sanitizer.discard();

        (text, elided > 0)
    }
}

/// How many bytes at the front of a window are the tail of a character that
/// began before it. At most three, by the shape of UTF-8.
fn leading_fragment(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take(3)
        .take_while(|byte| (*byte & 0xC0) == 0x80)
        .count()
}

/// The coalescing buffer behind `tool:progress` (PLAN 5.4), with one decoder
/// per pipe so a character split across chunks stays whole.
#[derive(Debug, Default)]
struct Frames {
    stdout: Frame,
    stderr: Frame,
    /// How much text is waiting to be sent.
    pending: usize,
    /// How much has been sent, against [`PROGRESS_MAX_BYTES`].
    emitted: u64,
}

impl Frames {
    /// Adds a chunk to the frame its pipe is building.
    fn push(&mut self, stream: Stream, bytes: &[u8]) {
        let frame = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        let decoded = frame.decoder.push(bytes);
        let text = frame.sanitizer.push(&decoded);
        self.pending = self.pending.saturating_add(text.len());
        frame.text.push_str(&text);
    }

    /// Sends whatever has accumulated, and empties the frames.
    fn flush(&mut self, progress: &dyn ProgressSink) {
        let mut emitted = self.emitted;
        for (stream, frame) in [
            (Stream::Stdout, &mut self.stdout),
            (Stream::Stderr, &mut self.stderr),
        ] {
            if frame.text.is_empty() {
                continue;
            }
            let text = std::mem::take(&mut frame.text);
            // Past the cap the pane stops; `truncated` on `tool:finished` says so.
            if emitted < PROGRESS_MAX_BYTES {
                emitted = emitted.saturating_add(text.len() as u64);
                progress.chunk(stream, &text);
            }
        }
        self.emitted = emitted;
        self.pending = 0;
    }

    /// Flushes, including what the decoders held.
    fn finish(&mut self, progress: &dyn ProgressSink) {
        for frame in [&mut self.stdout, &mut self.stderr] {
            let trailing = frame.decoder.finish();
            let text = frame.sanitizer.push(&trailing);
            frame.text.push_str(&text);
            frame.sanitizer.discard();
        }
        self.flush(progress);
    }
}

/// One pipe's half of a frame: [`Decoder`] makes text and [`Sanitizer`] strips
/// terminal control. Both keep state across chunks.
#[derive(Debug, Default)]
struct Frame {
    decoder: Decoder,
    sanitizer: Sanitizer,
    text: String,
}

/// Incremental decoding across chunks: UTF-8, holding back an incomplete
/// character. The first byte that cannot be UTF-8 switches the whole stream to
/// [`legacy_text`]: on Windows piped output is in the locale's code page, and a
/// program does not change encoding halfway.
#[derive(Debug, Default)]
struct Decoder {
    /// Bytes held back for the next chunk.
    carry: Vec<u8>,
    /// Whether this stream has proved it is not UTF-8.
    legacy: bool,
}

impl Decoder {
    /// Decodes as much as this chunk completes.
    fn push(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        if self.legacy {
            return self.take_legacy();
        }

        let Err(err) = std::str::from_utf8(&self.carry) else {
            let text = String::from_utf8_lossy(&self.carry).into_owned();
            self.carry.clear();
            return text;
        };

        let valid = err.valid_up_to();
        let mut text = std::str::from_utf8(&self.carry[..valid])
            .unwrap_or_default()
            .to_owned();
        self.carry.drain(..valid);

        // `None`: an incomplete character at the end, completed by the next
        // chunk. Anything else: the stream is not UTF-8.
        if err.error_len().is_some() {
            self.legacy = true;
            text.push_str(&self.take_legacy());
        }

        text
    }

    /// Decodes whatever is held, in the platform's legacy encoding.
    fn take_legacy(&mut self) -> String {
        let text = legacy_text(&self.carry);
        self.carry.clear();
        text
    }

    /// Decodes what is left when the pipe closes. A partial character becomes a
    /// replacement character rather than vanishing.
    fn finish(&mut self) -> String {
        if self.carry.is_empty() {
            return String::new();
        }
        if self.legacy {
            return self.take_legacy();
        }
        let text = String::from_utf8_lossy(&self.carry).into_owned();
        self.carry.clear();
        text
    }
}

/// Removes what a command wrote for a terminal: control sequences (colour,
/// cursor movement, window titles) and every control character but `\n` and
/// `\t`, from both the model's text and the pane.
///
/// Dropped rather than rendered: tools that colour also redraw progress bars
/// with cursor movement, and honouring that means writing a terminal emulator.
/// Dropping CR also stops CRLF from doubling line breaks.
#[derive(Debug, Default)]
struct Sanitizer {
    /// A control sequence that began at the end of a chunk, held until the
    /// rest of it arrives.
    pending: String,
}

/// The escape that starts every sequence.
const ESC: char = '\u{1b}';

/// Longest sequence held across chunks. Anything longer is a stray `ESC`, not
/// worth stalling the stream for.
const MAX_SEQUENCE: usize = 64;

impl Sanitizer {
    /// Cleans one chunk, holding back a sequence that is not finished.
    fn push(&mut self, text: &str) -> String {
        let mut source = std::mem::take(&mut self.pending);
        source.push_str(text);

        let mut out = String::with_capacity(source.len());
        let mut chars = source.chars();

        while let Some(ch) = chars.next() {
            if ch == ESC {
                if let Err(held) = skip_sequence(&mut chars) {
                    self.pending = held;
                    break;
                }
                continue;
            }
            // `\n` and `\t` are layout; other control characters are terminal
            // instructions.
            if ch == '\n' || ch == '\t' || !ch.is_control() {
                out.push(ch);
            }
        }

        out
    }

    /// Forgets a sequence the command ended mid-way: half an instruction is not
    /// text.
    fn discard(&mut self) {
        self.pending.clear();
    }
}

/// Consumes one control sequence, `ESC` already taken. `Err` returns what was
/// consumed when the chunk ended mid-sequence; past [`MAX_SEQUENCE`] it counts
/// as finished.
fn skip_sequence(chars: &mut std::str::Chars<'_>) -> Result<(), String> {
    let mut held = String::from(ESC);

    let Some(kind) = chars.next() else {
        return Err(held);
    };
    held.push(kind);

    // The two shapes with a terminator worth finding. Everything else — a
    // charset selection, `ESC c`, `ESC 7` — is two characters, both now taken.
    let terminated: fn(&str, char) -> bool = match kind {
        // CSI: parameters, then any byte in `@`..`~`. This is colour, cursor
        // movement, erasure — nearly everything in practice.
        '[' => |_, ch| ('\u{40}'..='\u{7e}').contains(&ch),
        // OSC: a string, then BEL or ST (`ESC \`).
        ']' => |held, ch| ch == '\u{7}' || (ch == '\\' && held.ends_with(ESC)),
        _ => return Ok(()),
    };

    loop {
        let Some(ch) = chars.next() else {
            return Err(held);
        };
        if terminated(&held, ch) {
            return Ok(());
        }
        held.push(ch);
        if held.len() > MAX_SEQUENCE {
            return Ok(());
        }
    }
}

/// Decodes non-UTF-8 bytes as programs write them **to a pipe**. On Windows
/// that is the ANSI code page (`GetACP`, 1252 on a Western install), not the
/// console's OEM page: a child with no console, as every child here is, uses
/// the locale's page (Python's `locale.getpreferredencoding()`, for one).
///
/// If this is ever wrong again, the symptom is accents arriving as other
/// accents (`é`→`Ú`, `û`→`¹`): cp1252 bytes read as cp850.
#[cfg(windows)]
fn legacy_text(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{GetACP, MultiByteToWideChar};

    /// Falls back to reading the bytes as UTF-8, damage and all. Reached only
    /// if the OS declines to decode its own code page.
    fn lossy(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    let Ok(len) = i32::try_from(bytes.len()) else {
        return lossy(bytes);
    };
    if len == 0 {
        return String::new();
    }

    // SAFETY: both calls are reads. The input pointer and length describe a
    // slice that outlives them; the first call passes a null output pointer
    // with a zero length, which is how this function is asked to measure; the
    // second passes a buffer of exactly the length the first returned.
    let (codepage, wide_len) = unsafe {
        let codepage = GetACP();
        (
            codepage,
            MultiByteToWideChar(codepage, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0),
        )
    };
    let Ok(wide_len) = usize::try_from(wide_len) else {
        return lossy(bytes);
    };
    if wide_len == 0 {
        return lossy(bytes);
    }

    let mut wide = vec![0u16; wide_len];
    // SAFETY: as above; `wide` holds exactly `wide_len` elements.
    let written = unsafe {
        MultiByteToWideChar(
            codepage,
            0,
            bytes.as_ptr(),
            len,
            wide.as_mut_ptr(),
            wide_len as i32,
        )
    };
    match usize::try_from(written) {
        Ok(written) if written > 0 => {
            wide.truncate(written);
            String::from_utf16_lossy(&wide)
        }
        _ => lossy(bytes),
    }
}

/// Decodes non-UTF-8 bytes. Off Windows the locale is UTF-8, so they are damage
/// and become replacement characters.
#[cfg(not(windows))]
fn legacy_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------
// Finding the program
// ---------------------------------------------------------------------------

/// Turns the program a caller named into a path to spawn. Shared with
/// [`mcp::client`](crate::mcp::client), so `npx` resolves the same way in both.
/// Done here rather than by `Command`, which would resolve relative to this
/// process's directory instead of `cwd`.
pub(crate) fn resolve(program: &str, cwd: &Path) -> Result<PathBuf, String> {
    let program = program.trim();
    let named = Path::new(program);

    // A name with a separator in it is a path, not a PATH lookup — the same
    // rule every shell uses.
    if crate::policy::grants::names_a_path(program) {
        let candidate = if named.is_absolute() {
            named.to_path_buf()
        } else {
            cwd.join(named)
        };
        return executable(&candidate).ok_or_else(|| {
            format!(
                "`{program}` is not a program that can be run from {}",
                cwd.display()
            )
        });
    }

    for directory in search_path() {
        if directory.as_os_str().is_empty() {
            continue;
        }
        if let Some(found) = executable(&directory.join(named)) {
            return Ok(found);
        }
    }

    Err(not_found(program))
}

/// The directories PATH names, in order.
fn search_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// Why a bare name found nothing.
fn not_found(program: &str) -> String {
    #[cfg(windows)]
    if is_cmd_builtin(program) {
        return format!(
            "`{program}` is a cmd.exe builtin, not a program on PATH. Run `cmd` with args \
             [\"/c\", \"{program}\", …] if you meant the builtin — note that cmd will then parse \
             those arguments itself."
        );
    }

    format!("`{program}` is not a program on PATH")
}

/// Whether a candidate path names something this platform can execute.
#[cfg(unix)]
fn executable(candidate: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;

    let meta = std::fs::metadata(candidate).ok()?;
    let runnable = meta.is_file() && meta.permissions().mode() & 0o111 != 0;
    runnable.then(|| candidate.to_path_buf())
}

/// Whether a candidate path names something this platform can execute: on
/// Windows by extension, trying `PATHEXT` in order, as a shell would
/// (PLAN 5.1).
#[cfg(windows)]
fn executable(candidate: &Path) -> Option<PathBuf> {
    if candidate.extension().is_some() && candidate.is_file() {
        return Some(candidate.to_path_buf());
    }

    for extension in path_extensions() {
        let mut named = candidate.as_os_str().to_owned();
        named.push(&extension);
        let with_extension = PathBuf::from(named);
        if with_extension.is_file() {
            return Some(with_extension);
        }
    }

    None
}

/// The suffixes `PATHEXT` names, or the documented default when it is unset.
#[cfg(windows)]
fn path_extensions() -> Vec<String> {
    const DEFAULT: &str = ".COM;.EXE;.BAT;.CMD";

    std::env::var("PATHEXT")
        .unwrap_or_else(|_| DEFAULT.to_owned())
        .split(';')
        .map(str::trim)
        .filter(|extension| !extension.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `cmd.exe` builtins, which are not on PATH. Only used for a better error.
#[cfg(windows)]
fn is_cmd_builtin(program: &str) -> bool {
    const BUILTINS: &[&str] = &[
        "assoc", "break", "call", "cd", "chdir", "cls", "color", "copy", "date", "del", "dir",
        "echo", "endlocal", "erase", "exit", "for", "ftype", "goto", "if", "md", "mkdir", "move",
        "path", "pause", "popd", "prompt", "pushd", "rd", "rem", "ren", "rename", "rmdir", "set",
        "setlocal", "shift", "start", "time", "title", "type", "ver", "verify", "vol",
    ];

    let name = program.trim().to_lowercase();
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(stem, _)| stem);
    BUILTINS.contains(&stem)
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
mod tests {
    use super::*;

    use crate::tools::NullProgress;

    /// A sink that keeps what it was given.
    #[derive(Debug, Default)]
    struct Recorder {
        chunks: std::sync::Mutex<Vec<(Stream, String)>>,
    }

    impl Recorder {
        fn text(&self) -> String {
            self.chunks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .map(|(_, text)| text.as_str())
                .collect()
        }

        fn streams(&self) -> Vec<Stream> {
            self.chunks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .map(|(stream, _)| *stream)
                .collect()
        }
    }

    impl ProgressSink for Recorder {
        fn chunk(&self, stream: Stream, text: &str) {
            self.chunks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((stream, text.to_owned()));
        }
    }

    #[test]
    fn a_capture_keeps_both_ends_and_says_what_it_dropped() {
        let mut capture = Capture::default();
        capture.push(&vec![b'a'; HEAD_BYTES]);
        capture.push(&vec![b'b'; 1000]);
        capture.push(&vec![b'z'; TAIL_BYTES]);

        let (text, truncated) = capture.render();

        assert!(truncated);
        assert!(text.starts_with("aaa"));
        assert!(text.ends_with("zzz"));
        assert!(text.contains("1000 bytes elided"), "{}", &text[..80]);
    }

    #[test]
    fn a_capture_under_the_cap_is_not_truncated() {
        let mut capture = Capture::default();
        capture.push(b"hello\n");

        assert_eq!(capture.render(), ("hello\n".to_owned(), false));
        assert_eq!(capture.total, 6);
    }

    #[test]
    fn a_character_split_across_chunks_is_decoded_once_it_is_whole() {
        let mut decoder = Decoder::default();
        let bytes = "é".as_bytes();

        assert_eq!(
            decoder.push(&bytes[..1]),
            "",
            "half a character says nothing"
        );
        assert_eq!(decoder.push(&bytes[1..]), "é");
        assert_eq!(decoder.finish(), "");
    }

    /// A byte that cannot be UTF-8 switches the stream to the platform encoding,
    /// as `cmd /c dir` output needs on a non-English Windows.
    #[test]
    fn a_byte_that_is_not_utf8_switches_the_stream_over() {
        let mut decoder = Decoder::default();
        let decoded = decoder.push(b"num\x82ro");

        assert!(decoder.legacy, "the stream is not UTF-8 and now knows it");
        assert!(decoded.starts_with("num"), "{decoded:?}");
    }

    /// Piped `numéro` comes back as `numéro`. The letter is asserted only under
    /// a Western code page; everywhere, no replacement character appears.
    #[cfg(windows)]
    #[test]
    fn piped_output_comes_back_as_the_letters_it_was() {
        // SAFETY: a parameterless read of a process-wide setting.
        let codepage = unsafe { windows_sys::Win32::Globalization::GetACP() };

        let decoded = legacy_text(b"num\xe9ro");
        assert!(
            !decoded.contains('\u{fffd}'),
            "the OS decoded its own code page: {decoded:?}"
        );

        if codepage == 1252 {
            assert_eq!(decoded, "numéro");
        }
    }

    /// The regression as found: cp1252 accents read as cp850 (`é`→`Ú`).
    #[cfg(windows)]
    #[test]
    fn accents_do_not_come_back_as_other_accents() {
        // SAFETY: a parameterless read of a process-wide setting.
        let codepage = unsafe { windows_sys::Win32::Globalization::GetACP() };
        if codepage != 1252 {
            return;
        }

        // `é û è €` as a Western program with no console writes them.
        let decoded = legacy_text(b"\xe9 \xfb \xe8 \x80");
        assert_eq!(decoded, "é û è €");

        for wrong in ['Ú', '¹', 'Þ'] {
            assert!(!decoded.contains(wrong), "cp850 crept back in: {decoded:?}");
        }
    }

    #[test]
    fn a_trailing_fragment_is_reported_rather_than_lost() {
        let mut decoder = Decoder::default();

        assert_eq!(decoder.push(&"é".as_bytes()[..1]), "");
        assert!(!decoder.legacy, "an unfinished character is not a verdict");
        assert_eq!(decoder.finish(), "\u{fffd}");
    }

    /// A tail cut out of the middle of a stream can begin inside a character.
    /// Those bytes are trimmed rather than shown as damage — and, crucially,
    /// that must not be mistaken for "this stream is not UTF-8".
    #[test]
    fn a_tail_that_starts_mid_character_does_not_condemn_the_stream() {
        let mut capture = Capture::default();
        capture.push(&vec![b'a'; HEAD_BYTES]);
        capture.push(&vec![b'b'; TAIL_BYTES]);
        // Lands at the front of the tail window, mid-character.
        capture.push("é rest".as_bytes());

        let (text, truncated) = capture.render();

        assert!(truncated);
        assert!(text.ends_with(" rest"), "{:?}", &text[text.len() - 16..]);
        assert!(
            !text.contains('\u{fffd}'),
            "a split character is trimmed, not rendered as damage"
        );
    }

    #[test]
    fn colour_is_removed_and_the_words_are_kept() {
        let mut sanitizer = Sanitizer::default();

        assert_eq!(
            sanitizer.push("\u{1b}[36mdist\u{1b}[0m  1"),
            "dist  1",
            "an ls in colour reads as an ls"
        );
    }

    /// A sequence can be split by wherever the pipe happened to break, and a
    /// frame that emitted the front half would print `[3` into the pane.
    #[test]
    fn a_sequence_split_across_chunks_is_still_removed() {
        let mut sanitizer = Sanitizer::default();

        assert_eq!(sanitizer.push("red: \u{1b}[3"), "red: ");
        assert_eq!(sanitizer.push("1merror\u{1b}[0m"), "error");
    }

    #[test]
    fn cursor_movement_and_window_titles_go_too() {
        let mut sanitizer = Sanitizer::default();

        // What a progress bar is made of, and what a build tool sets the
        // terminal title with.
        assert_eq!(sanitizer.push("\u{1b}[2K\u{1b}[1Gbuilding"), "building");
        assert_eq!(
            sanitizer.push("\u{1b}]0;a title\u{7}done"),
            "done",
            "an OSC string ends at BEL"
        );
        assert_eq!(
            sanitizer.push("\u{1b}]0;another\u{1b}\\after"),
            "after",
            "or at ST"
        );
    }

    /// Windows output is CRLF, and a lone CR before every newline would double
    /// every line break in the pane.
    #[test]
    fn carriage_returns_go_and_the_layout_stays() {
        let mut sanitizer = Sanitizer::default();

        assert_eq!(sanitizer.push("one\r\ntwo\r\n"), "one\ntwo\n");
        assert_eq!(
            sanitizer.push("a\tb\u{7}c"),
            "a\tbc",
            "tabs stay, BEL does not"
        );
    }

    /// A stray escape in output that is not a control sequence must not hold
    /// the rest of the stream hostage waiting for a terminator.
    #[test]
    fn a_sequence_that_never_ends_is_written_off() {
        let mut sanitizer = Sanitizer::default();
        let stray = format!("\u{1b}[{}", "9".repeat(MAX_SEQUENCE * 2));

        let out = sanitizer.push(&stray);

        assert!(sanitizer.pending.is_empty(), "nothing is still held");
        assert!(
            out.len() < stray.len(),
            "and the noise did not come through"
        );
    }

    /// An unfinished sequence at the end of a command is half an instruction,
    /// not text that was cut short.
    #[test]
    fn an_unfinished_sequence_is_dropped_rather_than_printed() {
        let recorder = Recorder::default();
        let mut frames = Frames::default();

        frames.push(Stream::Stdout, b"done\x1b[3");
        frames.finish(&recorder);

        assert_eq!(recorder.text(), "done");
    }

    /// The model reads the same cleaned text the pane shows, or the two would
    /// be looking at different output from the same command.
    #[test]
    fn the_envelope_is_cleaned_the_same_way_the_pane_is() {
        let mut capture = Capture::default();
        capture.push("\x1b[36mgreen\x1b[0m\r\n".as_bytes());

        assert_eq!(capture.render(), ("green\n".to_owned(), false));
        assert_eq!(
            capture.total, 16,
            "`bytes` still reports what the command actually produced"
        );
    }

    #[test]
    fn frames_keep_the_two_pipes_apart() {
        let recorder = Recorder::default();
        let mut frames = Frames::default();

        frames.push(Stream::Stdout, b"out");
        frames.push(Stream::Stderr, b"err");
        frames.finish(&recorder);

        assert_eq!(recorder.streams(), vec![Stream::Stdout, Stream::Stderr]);
        assert_eq!(recorder.text(), "outerr");
    }

    #[test]
    fn the_progress_stream_stops_at_its_cap() {
        let recorder = Recorder::default();
        let mut frames = Frames::default();

        // Well past the cap, in frames small enough that the cap is what stops
        // it rather than the size of any one of them.
        for _ in 0..40 {
            frames.push(Stream::Stdout, &vec![b'x'; 4096]);
            frames.flush(&recorder);
        }

        assert!(recorder.text().len() as u64 <= PROGRESS_MAX_BYTES + 4096);
    }

    #[test]
    fn a_bare_name_that_is_nowhere_is_named_in_the_failure() {
        let cwd = std::env::current_dir().expect("a working directory");
        let err =
            resolve("definitely-not-a-real-program-9k2", &cwd).expect_err("nothing resolves this");

        assert!(err.contains("definitely-not-a-real-program-9k2"), "{err}");
    }

    #[test]
    fn a_relative_program_is_resolved_against_the_working_directory() {
        let cwd = std::env::current_dir().expect("a working directory");
        let err = resolve("./nothing-here", &cwd).expect_err("nothing resolves this");

        assert!(err.contains(&cwd.display().to_string()), "{err}");
    }

    /// Tested against the diagnosis rather than through [`resolve`]: a machine
    /// with Git for Windows or the MSYS coreutils on PATH really does have an
    /// `echo.exe`, and that is a fact about the machine, not about the rule.
    #[cfg(windows)]
    #[test]
    fn a_cmd_builtin_is_diagnosed_rather_than_just_missing() {
        assert!(is_cmd_builtin("echo"));
        assert!(is_cmd_builtin("DIR"));
        assert!(!is_cmd_builtin("git"));

        let explained = not_found("echo");
        assert!(explained.contains("builtin"), "{explained}");
        assert!(explained.contains("/c"), "{explained}");
    }

    #[cfg(windows)]
    #[test]
    fn pathext_is_what_makes_a_bare_name_resolve() {
        let cwd = std::env::current_dir().expect("a working directory");
        let found = resolve("cmd", &cwd).expect("cmd.exe is on PATH");

        assert_eq!(
            found
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_lowercase),
            Some("exe".to_owned())
        );
    }

    /// A WSL host on a build without WSL refuses rather than running here.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_wsl_host_off_windows_refuses_rather_than_falling_back() {
        let target = ExecTarget {
            distro: "Ubuntu".to_owned(),
            cwd: "/home/p/proj".to_owned(),
        };
        let cwd = std::env::current_dir().expect("a working directory");

        let produced = exec(
            "true",
            &[],
            &cwd,
            Some(&target),
            Some(1000),
            &NullProgress,
            &CancellationToken::new(),
        )
        .await;
        let result = produced.result;

        assert!(!result.ok);
        assert_eq!(
            result.error.as_ref().map(|error| error.code),
            Some(ErrorCode::ExecHost)
        );
        assert!(
            result
                .error
                .as_ref()
                .is_some_and(|error| error.message.contains("Ubuntu")),
            "{result:?}"
        );
    }

    /// The first installed distribution. Tests that need one skip without it.
    #[cfg(windows)]
    async fn a_distro() -> Option<String> {
        crate::exec_host::installed().await.into_iter().next()
    }

    /// PLAN 7.12's exit: the command runs inside the distribution, in the
    /// directory it was given.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_command_runs_in_the_distro_and_in_the_directory_it_was_given() {
        let Some(distro) = a_distro().await else {
            return;
        };
        let target = ExecTarget {
            distro: distro.clone(),
            // Every distribution has `/etc`.
            cwd: "/etc".to_owned(),
        };
        let cwd = std::env::current_dir().expect("a working directory");

        let produced = exec(
            "pwd",
            &[],
            &cwd,
            Some(&target),
            Some(60_000),
            &NullProgress,
            &CancellationToken::new(),
        )
        .await;
        let result = produced.result;

        assert!(result.ok, "{result:?}");
        assert_eq!(result.content.trim(), "/etc", "{result:?}");
        assert_eq!(result.meta["exit_code"], 0);
        assert_eq!(result.meta["cwd"], "/etc");
        assert_eq!(
            result.meta["exec_host"], distro,
            "the record says which machine ran it"
        );
    }

    /// `wsl --cd` would silently start in `/` for a missing directory; the
    /// probe refuses instead.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_directory_the_distro_cannot_see_refuses_instead_of_running() {
        let Some(distro) = a_distro().await else {
            return;
        };
        let target = ExecTarget {
            distro,
            cwd: "/definitely-not-a-directory-9k2".to_owned(),
        };
        let cwd = std::env::current_dir().expect("a working directory");

        let produced = exec(
            "pwd",
            &[],
            &cwd,
            Some(&target),
            Some(60_000),
            &NullProgress,
            &CancellationToken::new(),
        )
        .await;
        let result = produced.result;

        assert!(!result.ok, "{result:?}");
        assert_eq!(
            result.error.as_ref().map(|error| error.code),
            Some(ErrorCode::ExecHost)
        );
        assert!(
            result.content.is_empty(),
            "nothing ran, so there is nothing it said: {result:?}"
        );
    }

    /// An unknown distribution is a host failure, not a non-zero exit.
    #[cfg(windows)]
    #[tokio::test]
    async fn an_unknown_distro_is_a_host_failure_and_not_an_exit_code() {
        if a_distro().await.is_none() {
            return;
        }
        let target = ExecTarget {
            distro: "definitely-not-a-distro-9k2".to_owned(),
            cwd: "/".to_owned(),
        };
        let cwd = std::env::current_dir().expect("a working directory");

        let produced = exec(
            "pwd",
            &[],
            &cwd,
            Some(&target),
            Some(60_000),
            &NullProgress,
            &CancellationToken::new(),
        )
        .await;
        let result = produced.result;

        assert!(!result.ok, "{result:?}");
        assert_eq!(
            result.error.as_ref().map(|error| error.code),
            Some(ErrorCode::ExecHost)
        );
        assert!(
            result
                .error
                .as_ref()
                .is_some_and(|error| error.message.contains("definitely-not-a-distro-9k2")),
            "{result:?}"
        );
    }

    #[test]
    fn sizes_read_as_sentences() {
        assert_eq!(bytes_phrase(0), "no output");
        assert_eq!(bytes_phrase(1), "1 byte of output");
        assert_eq!(bytes_phrase(2), "2 bytes of output");
    }
}
