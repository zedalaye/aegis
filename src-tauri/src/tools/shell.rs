//! The shell tool: `shell_exec`.
//!
//! The name is a misnomer inherited from the schema, and the first thing to
//! say about this module is what it does *not* do. There is no shell. The
//! program is looked up, spawned with its arguments as a vector, and that is
//! all: no `sh -c`, no `cmd /c` around an arbitrary string, so there is no
//! metacharacter layer to escape and none to defeat (PLAN 3.3, 5.1). Pipes,
//! redirection, globbing, `&&` and variable expansion simply are not features,
//! and the tool's description says so to the model rather than letting it
//! discover it through a confusing failure.
//!
//! What that buys is honesty in the approval dialog: the `program`, `args` and
//! `cwd` the user reads are literally what will be executed. It buys nothing
//! else. There is no sandbox — the command runs as the user, with the user's
//! environment (PLAN 3.3). The gate is the user's attention, and everything
//! here exists to keep that attention worth something.
//!
//! Four properties beyond that, each of which is a way a child process can go
//! wrong:
//!
//! * **It ends.** A command gets a deadline — the caller's, capped at 120
//!   seconds (PLAN 4.3) — and it is killed when the deadline passes.
//! * **It can be stopped.** The turn's cancellation token is awaited in the
//!   same `select!` as the pipes, so Stop kills the child rather than leaving
//!   it to finish into a turn nobody is listening to.
//! * **It cannot flood.** The envelope carries at most 64 KB of combined
//!   output — 48 KB of head, 16 KB of tail, an elision marker between them —
//!   and the `tool:progress` stream to the WebView is coalesced into ~50 ms
//!   frames and capped as well (PLAN 5.4). A command that prints a gigabyte is
//!   still drained, because a full pipe would block the child forever; it is
//!   just not repeated.
//! * **It is answered.** A program that exits non-zero is not a tool failure:
//!   `grep` finding nothing, `test` saying no and `cargo` reporting errors are
//!   all *results*. The envelope is `ok: true` with `meta.exit_code`, and the
//!   output is preserved — which is the point, since the output is where the
//!   error message is. `ok: false` is reserved for the cases where the command
//!   did not run or did not finish: it could not be found or spawned, it hit
//!   the deadline, or the turn was cancelled.
//!
//! ## Windows
//!
//! `CreateProcess` cannot launch a `.cmd` or `.bat` shim, which is what
//! `pnpm`, `npm` and `yarn` are on Windows (PLAN 5.1). Resolution therefore
//! walks `PATHEXT`, so a bare `pnpm` finds `pnpm.cmd`. Launching it goes
//! through the standard library rather than a hand-written `cmd /c`: since
//! 1.77.2 `Command` recognizes a batch target, invokes it through `cmd.exe`
//! itself, and escapes the arguments for `cmd`'s own parser — which is the
//! part that matters. Writing the `cmd /c` by hand would hand `cmd` an
//! argument string quoted for the C runtime instead, and `&`, `|` and `^`
//! inside a model-supplied argument would become operators. The observable
//! behaviour is what PLAN 5.1 asks for; the escaping is the library's, on
//! purpose.
//!
//! Children are also spawned with `CREATE_NO_WINDOW`, or every call flashes a
//! console window on the user's screen.

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
use crate::policy::matrix::shell_line;
use crate::policy::tool;

/// The longest deadline a caller may ask for, in milliseconds (PLAN 4.1).
///
/// Also the default. A model that names no deadline gets the ceiling rather
/// than some shorter number invented here: the user is watching the output
/// arrive and can stop it, and a tool that killed `cargo build` after ten
/// seconds would be a tool nobody could use for the one thing it is for.
pub const TIMEOUT_CEILING_MS: u64 = 120_000;

/// How much of the output is kept from the start (PLAN 4.3).
const HEAD_BYTES: usize = 48 * 1024;

/// How much is kept from the end (PLAN 4.3).
const TAIL_BYTES: usize = 16 * 1024;

/// How long a `tool:progress` frame stays open (PLAN 5.4).
///
/// The same 50 ms as `turn:delta`, for the same reason: waking the WebView per
/// pipe read is what makes a streaming pane slower than a batched one.
const PROGRESS_FRAME: Duration = Duration::from_millis(50);

/// Most bytes of output the progress stream will carry to the WebView.
///
/// The pane is a live view, not a record: past this the envelope and the audit
/// line are what remain, and `truncated` on `tool:finished` tells the UI to
/// say so. Matching [`EXEC_MAX_BYTES`] keeps the two limits one number.
const PROGRESS_MAX_BYTES: u64 = EXEC_MAX_BYTES;

/// How much text may accumulate in a frame before it is sent early.
///
/// A command that prints faster than 50 ms of frames can carry should still
/// arrive smoothly rather than in one 64 KB jolt at the end.
const FRAME_MAX_BYTES: usize = 8 * 1024;

/// How much is read from a pipe at a time.
const PIPE_CHUNK: usize = 8 * 1024;

/// Chunks the two pump tasks may queue before they wait.
///
/// Backpressure by design: a child printing faster than this loop can account
/// for it is a child that gets blocked on its own `write`, which is the
/// correct thing to happen and is what stops memory growing without bound.
const PIPE_QUEUE: usize = 16;

/// `CREATE_NO_WINDOW` — the child gets no console (PLAN 5.1).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

/// Runs a program and collects what it said.
///
/// `cwd` was resolved and judged by [`policy`](crate::policy); `program` was
/// not, and is resolved here — that is the one path decision this module makes
/// and it is deliberate. Containment does not apply to it: the row the user
/// approved is about the *working directory*, and `git` living in `/usr/bin`
/// is not a fact anyone was asked about.
pub(crate) async fn exec(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout_ms: Option<u64>,
    progress: &dyn ProgressSink,
    cancel: &CancellationToken,
) -> Produced {
    let line = shell_line(program, args);

    let resolved = match resolve(program, cwd) {
        Ok(path) => path,
        Err(message) => return Produced::failed(tool::SHELL_EXEC, ErrorCode::ToolFailed, message),
    };

    let budget = Duration::from_millis(
        timeout_ms
            .unwrap_or(TIMEOUT_CEILING_MS)
            .clamp(1, TIMEOUT_CEILING_MS),
    );

    let mut command = Command::new(&resolved);
    command
        .args(args)
        .current_dir(cwd)
        // A command that reads stdin would otherwise wait for input nobody can
        // type, and look exactly like a hang until the deadline killed it.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The last defence against an orphan: if this future is dropped
        // without reaching the kill below, the child still goes.
        .kill_on_drop(true);

    // `tokio::process::Command` carries this natively on Windows; without it
    // every call flashes a console window in the user's face (PLAN 5.1).
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!(%err, program = %resolved.display(), "a command would not start");
            return Produced::failed(
                tool::SHELL_EXEC,
                ErrorCode::ToolFailed,
                spawn_failure(&resolved, &err),
            );
        }
    };

    tracing::info!(program = %resolved.display(), cwd = %cwd.display(), "running a command");

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
    let meta = json!({
        "program": resolved.display().to_string(),
        "cwd": cwd.display().to_string(),
        "duration_ms": duration_ms,
        "exit_code": ended.exit_code(),
        "bytes": capture.total,
    });

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
        // The pipes closed and the process could not be reaped. Rare enough
        // that the honest answer is to say what is and is not known rather
        // than to invent an exit code.
        Ended::Lost(message) => Produced::failed(
            tool::SHELL_EXEC,
            ErrorCode::ToolFailed,
            format!("`{line}` ran, but its result could not be read: {message}"),
        ),
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
    /// The exit code for the envelope's `meta`, when there is one.
    ///
    /// `None` for a command that was killed, and also for one terminated by a
    /// signal — in both cases there is no code, and a `0` invented to fill the
    /// field would read as success.
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
    /// Drains the child's pipes to the end, or to whichever deadline arrives
    /// first, and reaps it.
    ///
    /// Both pipes are read by their own task and funnelled into one channel,
    /// so the two are interleaved in arrival order — which is what a terminal
    /// shows, and what makes a compiler's diagnostics line up with the
    /// progress it printed. The distinction is not lost: every frame carries
    /// which pipe it came from.
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

        // `biased` so a cancel that lands alongside a chunk wins: the user
        // pressed Stop, and output emitted afterwards is output from a command
        // they stopped.
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

        // Reaching here with `None` means both pipes are at EOF, which for
        // almost every program means it has exited. Almost: a child that
        // spawned a grandchild holding the pipes open has already closed its
        // own, and one that closed them and kept working has not exited at
        // all — so the wait is still under the same two deadlines.
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

    /// Kills a command that has run out of deadline or out of turn.
    ///
    /// On Windows the tree goes, not just the process. Every `.cmd` shim is
    /// launched through a `cmd.exe` that is not itself doing the work, and
    /// Windows does not take a process' children with it — so killing only the
    /// child would leave the actual build running, holding the pipes open, and
    /// the Stop button would be a lie. `taskkill` is the tool the platform
    /// provides for this; the alternative is a job object, which is a
    /// meaningful amount of `unsafe` for the same effect.
    ///
    /// On Unix a shell `exec`s its last command, so killing the child kills
    /// what the user was told would run in the ordinary case. A script that
    /// spawns and then waits is not covered; that needs process groups, and it
    /// is noted in the README rather than half-done here.
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

            // A non-zero status is ordinary: it means the tree was already
            // gone. Only a `taskkill` that would not run at all is worth a
            // line, and the `kill` below is the answer either way.
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

/// Reads one pipe to EOF, forwarding what it finds.
///
/// Errors end the pump rather than being reported: a broken pipe is what a
/// killed child looks like from here, and there is nothing the turn would do
/// with the distinction that it is not already doing.
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

/// The bounded copy of a command's output (PLAN 4.3).
///
/// Head and tail rather than a plain prefix, because the two ends of a build
/// log are the useful parts: what it started doing, and how it ended. The
/// middle is where the repetition lives.
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

        // A chunk longer than the whole tail window replaces it outright,
        // which keeps the per-chunk cost bounded by the chunk rather than by
        // how much has been thrown away so far.
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

    /// The text for the envelope, and whether anything was dropped.
    ///
    /// Decoded, never refused. `fs_read` reports a binary file as binary
    /// because reading one is a mistake worth naming; a command's output is a
    /// different thing — it is text, possibly in the platform's legacy
    /// encoding, and the lines are still the answer. The same [`Decoder`] the
    /// live pane uses does the work, so the two cannot disagree about what a
    /// command said, and one decoder spans both halves so the encoding it
    /// settled on for the head also applies to the tail.
    ///
    /// The elision marker between the halves is inside the content as well as
    /// on the envelope, so a model that only reads the text still learns that
    /// a middle is missing.
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
            // The tail is cut out of the middle of a stream, so in UTF-8 it
            // can begin inside a character. Those bytes are dropped rather
            // than shown as damage — but only while the stream still reads as
            // UTF-8, because in a legacy encoding the very same bytes are
            // ordinary letters.
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

/// The coalescing buffer behind `tool:progress` (PLAN 5.4).
///
/// One decoder per pipe, because a chunk boundary can fall inside a multi-byte
/// character and a frame that split one would put a replacement character in
/// the middle of a word that is not broken.
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
            // Past the cap the pane stops filling. It is a live view of a
            // running command, not the record — the envelope and the audit
            // line are — and `truncated` on `tool:finished` is what tells the
            // UI to say so.
            if emitted < PROGRESS_MAX_BYTES {
                emitted = emitted.saturating_add(text.len() as u64);
                progress.chunk(stream, &text);
            }
        }
        self.emitted = emitted;
        self.pending = 0;
    }

    /// Flushes, including whatever the decoders were still holding.
    ///
    /// A trailing incomplete character is emitted as a replacement rather than
    /// dropped: the bytes were on the pipe, and a pane that silently loses the
    /// last character of a command's output is one nobody can trust.
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

/// One pipe's half of a frame.
///
/// Two stages, because they answer two different questions: [`Decoder`] turns
/// bytes into text, [`Sanitizer`] decides which of that text is meant for a
/// terminal rather than for a reader. Both are per-pipe and both carry state
/// across chunks, since either a character or a control sequence can be split
/// by wherever the pipe happened to break.
#[derive(Debug, Default)]
struct Frame {
    decoder: Decoder,
    sanitizer: Sanitizer,
    text: String,
}

/// Incremental decoding across chunk boundaries.
///
/// UTF-8 first, because that is what the tools anyone runs an agent against
/// emit. It holds back at most three bytes — an incomplete character at the
/// end of a chunk — and hands them to the next one, so a frame boundary never
/// splits a word.
///
/// A byte that cannot be UTF-8 at all is not damage to be papered over with a
/// replacement character: on Windows it is the ordinary case, because a program
/// writing to a pipe encodes in the locale's code page. So the first such byte
/// switches the decoder to [`legacy_text`] for the rest of the stream. That is a
/// per-stream decision rather than a per-chunk one because a program does not
/// change encoding halfway through, and because guessing again on every chunk
/// would make the answer depend on where the pipe happened to break.
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

        // `None` is an incomplete character at the very end: keep it, the rest
        // of it is in the next chunk. Anything else means this is not UTF-8 at
        // all, and everything from here on is read the other way.
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

    /// Decodes whatever is left when the pipe closes.
    ///
    /// A trailing incomplete character is emitted as a replacement rather than
    /// dropped: the bytes were on the pipe, and a pane that silently loses the
    /// last character of a command's output is one nobody can trust.
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

/// Removes what a command wrote for a terminal rather than for a reader.
///
/// Output on a pipe carries two kinds of thing. One is text. The other is
/// control sequences — `ESC[36m` to turn the next word cyan, `ESC[2K` to erase
/// a line, `ESC]0;…BEL` to retitle a window — which mean something to a
/// terminal emulator and nothing anywhere else. Aegis' transcript is not a
/// terminal, so they are dropped from both what the model reads and what the
/// pane shows.
///
/// Dropping rather than rendering is a decision, and the reason is the second
/// kind of sequence rather than the first. Colour alone would be easy to
/// render; but the tools that emit colour — `cargo`, `pnpm`, `docker` — emit
/// cursor movement in the same breath, to draw a progress bar by erasing and
/// redrawing one line. A pane that honoured the colours and ignored the
/// movement would show every frame of that bar stacked on top of each other,
/// which is worse than plain text, and honouring the movement means writing a
/// terminal emulator. Plain text is the honest floor.
///
/// Carriage returns go with them, which also fixes the ordinary Windows case:
/// output is CRLF, and a lone `CR` before every newline would double every
/// line break in the pane. Only `\n` and `\t` survive as control characters.
#[derive(Debug, Default)]
struct Sanitizer {
    /// A control sequence that began at the end of a chunk, held until the
    /// rest of it arrives.
    pending: String,
}

/// The escape that starts every sequence.
const ESC: char = '\u{1b}';

/// Longest sequence held across chunks before it is written off.
///
/// A real one is a handful of characters. Something longer is a stray `ESC` in
/// the middle of output that is not a sequence at all, and holding the rest of
/// the stream hostage waiting for a terminator that will never come is the one
/// failure mode this guard exists to prevent.
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
            // `\n` and `\t` are layout a reader needs. Every other control
            // character — CR, BEL, backspace, form feed — is an instruction to
            // a terminal.
            if ch == '\n' || ch == '\t' || !ch.is_control() {
                out.push(ch);
            }
        }

        out
    }

    /// Forgets a sequence the command ended in the middle of.
    ///
    /// Nothing is emitted: an unfinished `ESC[3` is not text that was cut
    /// short, it is half an instruction, and printing it would put exactly the
    /// noise this type exists to remove into the last line of the pane.
    fn discard(&mut self) {
        self.pending.clear();
    }
}

/// Consumes one control sequence, `ESC` already taken.
///
/// `Err` carries back everything consumed so far, for a sequence the chunk
/// ended in the middle of. A sequence that runs past [`MAX_SEQUENCE`] is
/// treated as finished — dropped, rather than held for a terminator that is
/// not coming.
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

/// Decodes bytes that are not UTF-8, in whatever this platform's programs
/// actually write **to a pipe**.
///
/// On Windows that is the system **ANSI** code page — 1252 on a Western
/// install — and the emphasis is the whole of this function's history. Windows
/// has two legacy code pages, and which one a program writes depends on what it
/// is writing *to*: the OEM page (850 here, 437 on a US box) is the console's,
/// and the ANSI page is the locale's. A program with no console attached takes
/// the second. `shell_exec` pipes both streams and never gives a child a
/// console, so that is always the case here.
///
/// This read `GetOEMCP` first, on the grounds that `cmd`'s built-ins write the
/// OEM page redirected or not — which is true, and beside the point: this tool
/// spawns programs directly with no shell, so a built-in is only reachable
/// through an explicit `cmd /c`. What it actually runs is CRT and interpreter
/// programs, and those pick the locale's page. Python is the clearest case: for
/// a non-console stdout it encodes with `locale.getpreferredencoding()`, which
/// on Windows is the ANSI page.
///
/// The symptom that found it, and the one to recognise if this is ever wrong
/// again: accented letters arriving as *other* accented letters, consistently —
/// `é`→`Ú`, `û`→`¹`, `è`→`Þ`. Those are exactly the cp1252 bytes `0xE9`, `0xFB`
/// and `0xE8` read as cp850. Not replacement characters, which is why it reads
/// as a broken font or a broken PDF rather than as a decoding bug, and why the
/// model that hit it spent two rounds blaming its extraction library.
///
/// `GetACP` rather than `GetConsoleOutputCP` for the reason the old comment
/// gave about `GetOEMCP`: a windowed application has no console to report one.
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

/// Decodes bytes that are not UTF-8.
///
/// Everywhere but Windows there is nothing better to try: the locale is UTF-8
/// on any machine this runs on, so a byte that is not UTF-8 is damage rather
/// than another encoding, and saying so with a replacement character is the
/// honest answer.
#[cfg(not(windows))]
fn legacy_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------
// Finding the program
// ---------------------------------------------------------------------------

/// Turns the program a caller named into a path to spawn.
///
/// Shared with [`mcp::client`](crate::mcp::client) since Phase 18, which needs
/// exactly the same answer for exactly the same reason: on Windows `npx` is a
/// `.cmd` and `CreateProcess` cannot launch one, so a connector configured the
/// way every MCP host documents would simply never start. One resolver rather
/// than two, so "which npx" cannot mean different things in two places.
///
/// Resolved here rather than left to `Command`, which searches PATH relative
/// to *this* process' working directory and not to `current_dir` — so a
/// relative program name would find something different from what the user
/// read in the dialog, or nothing at all, depending on the platform. Doing it
/// explicitly makes the answer the same everywhere and lets the failure say
/// which of the two things went wrong.
pub(crate) fn resolve(program: &str, cwd: &Path) -> Result<PathBuf, String> {
    let program = program.trim();
    let named = Path::new(program);

    // A name with a separator in it is a path, not a PATH lookup — the same
    // rule every shell uses.
    let has_separator = named
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty());

    if has_separator {
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

/// Whether a candidate path names something this platform can execute.
///
/// Windows decides by extension, and a bare name is not a file: `pnpm` has to
/// be tried as `pnpm.exe`, `pnpm.cmd` and whatever else `PATHEXT` lists, in
/// that order, because that order is what the user's own shell would use
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

/// Names `cmd.exe` handles itself, which are therefore not on PATH.
///
/// Only used to write a better error. `echo` and `dir` are the two a model
/// reaches for first, and "not a program on PATH" is a true but unhelpful
/// thing to tell it about them.
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
        // What the standard library returns for a batch file whose arguments
        // cannot be escaped safely for `cmd.exe`. Refusing is the correct
        // answer; saying why is this function's job.
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

    /// A byte that cannot be UTF-8 settles the question for the whole stream:
    /// this is text in the platform's own encoding, not UTF-8 with damage in
    /// it. That is exactly what `cmd /c dir` looks like on any non-English
    /// Windows.
    #[test]
    fn a_byte_that_is_not_utf8_switches_the_stream_over() {
        let mut decoder = Decoder::default();
        let decoded = decoder.push(b"num\x82ro");

        assert!(decoder.legacy, "the stream is not UTF-8 and now knows it");
        assert!(decoded.starts_with("num"), "{decoded:?}");
    }

    /// The point of the switch, on the platform that needs it: a Python script
    /// printing `numéro` to a pipe comes back as `numéro`, not `num?ro` and not
    /// `numÚro`.
    ///
    /// `0xE9` is the byte, because that is what the locale's code page uses for
    /// `é` and the locale's page is what a program with no console writes —
    /// which is every program this tool runs, since it pipes both streams. The
    /// exact letter is asserted only where that page is a Western one;
    /// elsewhere the OS still decodes, and "no replacement character" holds.
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

    /// The regression, in the shape it was actually found in.
    ///
    /// A PDF extractor printing accented text through a pipe came back with
    /// every accent mapped to a *different* accented letter — `é`→`Ú`, `û`→`¹`,
    /// `è`→`Þ` — which is cp1252 bytes read as cp850, the console's page rather
    /// than the locale's. No replacement characters anywhere, which is why it
    /// reads as a broken font rather than as a decoding bug.
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

    #[test]
    fn sizes_read_as_sentences() {
        assert_eq!(bytes_phrase(0), "no output");
        assert_eq!(bytes_phrase(1), "1 byte of output");
        assert_eq!(bytes_phrase(2), "2 bytes of output");
    }
}
