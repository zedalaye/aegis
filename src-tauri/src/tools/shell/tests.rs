use super::program::*;
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
