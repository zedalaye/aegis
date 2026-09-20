//! What the model sees of a command's output: bounded, decoded, stripped of
//! terminal control sequences (PLAN 4.3, PLAN 5.4).

use super::*;

/// The bounded copy of a command's output (PLAN 4.3): head and tail, the useful
/// ends of a build log.
#[derive(Debug, Default)]
pub(super) struct Capture {
    /// The first [`HEAD_BYTES`].
    pub(super) head: Vec<u8>,
    /// The last [`TAIL_BYTES`] of everything after the head.
    pub(super) tail: VecDeque<u8>,
    /// Everything the command produced, kept or not.
    pub(super) total: u64,
}

impl Capture {
    /// Accounts for one chunk, keeping the ends and dropping the middle.
    pub(super) fn push(&mut self, bytes: &[u8]) {
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
    pub(super) fn render(&self) -> (String, bool) {
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
pub(super) fn leading_fragment(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take(3)
        .take_while(|byte| (*byte & 0xC0) == 0x80)
        .count()
}

/// The coalescing buffer behind `tool:progress` (PLAN 5.4), with one decoder
/// per pipe so a character split across chunks stays whole.
#[derive(Debug, Default)]
pub(super) struct Frames {
    pub(super) stdout: Frame,
    pub(super) stderr: Frame,
    /// How much text is waiting to be sent.
    pub(super) pending: usize,
    /// How much has been sent, against [`PROGRESS_MAX_BYTES`].
    pub(super) emitted: u64,
}

impl Frames {
    /// Adds a chunk to the frame its pipe is building.
    pub(super) fn push(&mut self, stream: Stream, bytes: &[u8]) {
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
    pub(super) fn flush(&mut self, progress: &dyn ProgressSink) {
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
    pub(super) fn finish(&mut self, progress: &dyn ProgressSink) {
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
pub(super) struct Frame {
    pub(super) decoder: Decoder,
    pub(super) sanitizer: Sanitizer,
    pub(super) text: String,
}

/// Incremental decoding across chunks: UTF-8, holding back an incomplete
/// character. The first byte that cannot be UTF-8 switches the whole stream to
/// [`legacy_text`]: on Windows piped output is in the locale's code page, and a
/// program does not change encoding halfway.
#[derive(Debug, Default)]
pub(super) struct Decoder {
    /// Bytes held back for the next chunk.
    pub(super) carry: Vec<u8>,
    /// Whether this stream has proved it is not UTF-8.
    pub(super) legacy: bool,
}

impl Decoder {
    /// Decodes as much as this chunk completes.
    pub(super) fn push(&mut self, bytes: &[u8]) -> String {
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
    pub(super) fn take_legacy(&mut self) -> String {
        let text = legacy_text(&self.carry);
        self.carry.clear();
        text
    }

    /// Decodes what is left when the pipe closes. A partial character becomes a
    /// replacement character rather than vanishing.
    pub(super) fn finish(&mut self) -> String {
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
pub(super) struct Sanitizer {
    /// A control sequence that began at the end of a chunk, held until the
    /// rest of it arrives.
    pub(super) pending: String,
}

/// The escape that starts every sequence.
pub(super) const ESC: char = '\u{1b}';

/// Longest sequence held across chunks. Anything longer is a stray `ESC`, not
/// worth stalling the stream for.
pub(super) const MAX_SEQUENCE: usize = 64;

impl Sanitizer {
    /// Cleans one chunk, holding back a sequence that is not finished.
    pub(super) fn push(&mut self, text: &str) -> String {
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
    pub(super) fn discard(&mut self) {
        self.pending.clear();
    }
}

/// Consumes one control sequence, `ESC` already taken. `Err` returns what was
/// consumed when the chunk ended mid-sequence; past [`MAX_SEQUENCE`] it counts
/// as finished.
pub(super) fn skip_sequence(chars: &mut std::str::Chars<'_>) -> Result<(), String> {
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
pub(super) fn legacy_text(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{GetACP, MultiByteToWideChar};

    /// Falls back to reading the bytes as UTF-8, damage and all. Reached only
    /// if the OS declines to decode its own code page.
    pub(super) fn lossy(bytes: &[u8]) -> String {
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
pub(super) fn legacy_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
