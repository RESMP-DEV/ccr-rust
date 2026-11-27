// SPDX-License-Identifier: AGPL-3.0-or-later
use super::SseFrame;

/// Incremental SSE decoder that accepts arbitrary byte chunks.
#[derive(Debug, Default)]
pub struct SseFrameDecoder {
    buf: Vec<u8>,
    /// Set after an oversized frame was discarded. The decoder then skips
    /// bytes through the next frame boundary so the tail of a malformed
    /// frame is never parsed or forwarded as if it were a new frame.
    discard_until_boundary: bool,
    /// Line position of the resync scan inside the discarded frame.
    discard_state: DiscardState,
    /// Number of frames and frame tails this decoder has dropped.
    dropped_frame_count: u64,
}

/// Where the resync scan is relative to the discarded frame's line
/// structure. A discarded frame can end at any byte, so the next chunk may
/// continue a line, start a new line, or complete a CRLF pair whose CR was
/// already consumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum DiscardState {
    /// The next byte continues a non-empty line of the discarded frame.
    MidLine,
    /// The next byte begins a new line.
    #[default]
    LineStart,
    /// The previous buffer ended with a carriage return. A leading line feed
    /// completes that terminator and must not be read as an empty line.
    SwallowLf,
    /// The previous buffer ended with the carriage return of an empty line.
    /// A leading line feed completes that blank-line boundary, and parsing
    /// resumes immediately after it.
    BoundaryCr,
}

/// Maximum bytes retained while waiting for one SSE frame boundary. This
/// matches the first-event peek limit and prevents a provider that emits an
/// unterminated frame from growing router memory until stream idle timeout.
pub(super) const MAX_PENDING_BYTES: usize = 1024 * 1024;

impl SseFrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push bytes and emit complete frames that end with a blank line.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        if !chunk.is_empty() {
            self.buf.extend_from_slice(chunk);
        }

        if self.discard_until_boundary && !self.resync() {
            return Vec::new();
        }

        let mut frames = Vec::new();
        let mut frame_start = 0;

        while let Some((frame_end, next_start)) = find_frame_boundary(&self.buf, frame_start) {
            if let Some(frame) = parse_frame(&self.buf[frame_start..frame_end]) {
                frames.push(frame);
            }
            frame_start = next_start;
        }

        if frame_start > 0 {
            self.buf.drain(..frame_start);
        }

        // The limit applies to the incomplete frame still buffered, never to
        // a batch of already complete frames: parsing happens first so a
        // large chunk of valid frames is never discarded.
        if self.buf.len() > MAX_PENDING_BYTES {
            let discarded = self.buf.len();
            self.discard_state = match self.buf.last() {
                Some(b'\n') => DiscardState::LineStart,
                Some(b'\r') => {
                    let last = self.buf.len() - 1;
                    if last == 0 || matches!(self.buf[last - 1], b'\n' | b'\r') {
                        DiscardState::BoundaryCr
                    } else {
                        DiscardState::SwallowLf
                    }
                }
                _ => DiscardState::MidLine,
            };
            self.buf.clear();
            self.discard_until_boundary = true;
            self.note_dropped_frame();
            tracing::warn!(
                discarded_bytes = discarded,
                limit_bytes = MAX_PENDING_BYTES,
                "Discarding malformed SSE frame with no boundary"
            );
        }

        frames
    }

    /// Skip the discarded frame's tail through the first blank line.
    ///
    /// Returns `true` once the boundary has been consumed and the remaining
    /// buffer starts at the next frame. The scan tracks whether it is
    /// mid-line, so a line feed completing a CRLF pair is never mistaken for
    /// an empty line; that mistake resumed parsing one byte inside the tail.
    fn resync(&mut self) -> bool {
        let mut cursor = 0;

        if self.discard_state == DiscardState::BoundaryCr {
            if self.buf.first() == Some(&b'\n') {
                self.buf.drain(..1);
            }
            self.discard_until_boundary = false;
            self.discard_state = DiscardState::LineStart;
            return true;
        }

        if self.discard_state == DiscardState::SwallowLf {
            if self.buf.is_empty() {
                return false;
            }
            if self.buf.first() == Some(&b'\n') {
                cursor = 1;
            }
            self.discard_state = DiscardState::LineStart;
            if cursor == self.buf.len() {
                self.buf.clear();
                return false;
            }
        }

        if self.discard_state == DiscardState::MidLine {
            match read_next_line(&self.buf, cursor) {
                Some((_, next)) => {
                    cursor = next;
                    self.discard_state = DiscardState::LineStart;
                }
                None => {
                    if self.buf.last() == Some(&b'\r') {
                        self.discard_state = DiscardState::SwallowLf;
                    }
                    self.buf.clear();
                    return false;
                }
            }
        }

        while cursor < self.buf.len() {
            let line_start = cursor;
            match read_next_line(&self.buf, cursor) {
                Some((line_end, next)) => {
                    if line_end == line_start {
                        self.buf.drain(..next);
                        self.finish_resync();
                        return true;
                    }
                    cursor = next;
                }
                None => {
                    if self.buf[cursor] == b'\r' {
                        // An empty line terminated by a bare CR is a frame
                        // boundary. Consume the CR and let the normal parser
                        // treat a following LF as the empty line it completes.
                        self.buf.drain(..cursor + 1);
                        self.finish_resync();
                        return true;
                    }
                    let ends_with_cr = self.buf.last() == Some(&b'\r');
                    self.buf.clear();
                    self.discard_state = if ends_with_cr {
                        DiscardState::SwallowLf
                    } else {
                        DiscardState::MidLine
                    };
                    return false;
                }
            }
        }

        self.buf.clear();
        false
    }

    fn finish_resync(&mut self) {
        self.discard_until_boundary = false;
        self.discard_state = DiscardState::LineStart;
        self.note_dropped_frame();
    }

    fn note_dropped_frame(&mut self) {
        self.dropped_frame_count = self.dropped_frame_count.saturating_add(1);
    }

    /// Flush a final frame whose blank-line terminator never arrived.
    ///
    /// A buffer ending in a line terminator is a complete frame with only the
    /// blank separator missing, so it is parsed and emitted. A buffer ending
    /// mid-line is not a frame and stays discarded, as does anything left by
    /// an oversized frame in `discard_until_boundary` mode. A multi-line tail
    /// whose last line was cut is dropped whole instead of forwarding only
    /// the completed data lines.
    pub fn finish(&mut self) -> Vec<SseFrame> {
        if self.discard_until_boundary {
            self.buf.clear();
            return Vec::new();
        }

        if self.buf.is_empty() {
            return Vec::new();
        }

        match classify_tail(&self.buf) {
            TailStatus::Complete => {
                // A trailing bare CR is a line terminator, not part of the
                // last field value. Normalize it so parse_frame applies the
                // same line boundary read_next_line does.
                if self.buf.last() == Some(&b'\r') {
                    let last = self.buf.len() - 1;
                    self.buf[last] = b'\n';
                }
                let frame = parse_frame(&self.buf);
                self.buf.clear();
                frame.into_iter().collect()
            }
            TailStatus::MidLine | TailStatus::TruncatedMultiline => {
                self.buf.clear();
                self.note_dropped_frame();
                Vec::new()
            }
        }
    }

    pub fn pending_bytes(&self) -> usize {
        self.buf.len()
    }

    /// Number of frames and frame tails this decoder has dropped.
    ///
    /// An oversized frame counts once when it is dropped and once more when
    /// the resync skips its tail. A mid-line or truncated multi-line tail that
    /// `finish()` refuses to emit also counts.
    pub fn dropped_frame_count(&self) -> u64 {
        self.dropped_frame_count
    }
}

fn find_frame_boundary(buf: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut cursor = start;
    while cursor < buf.len() {
        let line_start = cursor;
        let (line_end, next_cursor) = read_next_line(buf, cursor)?;
        if line_end == line_start {
            return Some((line_start, next_cursor));
        }
        cursor = next_cursor;
    }
    None
}

pub(super) fn read_next_line(buf: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut i = start;
    while i < buf.len() {
        match buf[i] {
            b'\n' => return Some((i, i + 1)),
            b'\r' => {
                if i + 1 == buf.len() {
                    return None;
                }
                if buf[i + 1] == b'\n' {
                    return Some((i, i + 2));
                }
                return Some((i, i + 1));
            }
            _ => i += 1,
        }
    }
    None
}

fn parse_frame(frame_bytes: &[u8]) -> Option<SseFrame> {
    let mut event = None;
    let mut data_lines = Vec::new();
    let mut cursor = 0;

    while cursor < frame_bytes.len() {
        let line_start = cursor;
        let (line_end, next_cursor) = match read_next_line(frame_bytes, cursor) {
            Some(bounds) => bounds,
            None => {
                let line_end = if frame_bytes.last() == Some(&b'\r') {
                    frame_bytes.len() - 1
                } else {
                    frame_bytes.len()
                };
                (line_end, frame_bytes.len())
            }
        };

        let line = &frame_bytes[line_start..line_end];
        if line.is_empty() || line.starts_with(b":") {
            cursor = next_cursor;
            continue;
        }

        if let Some(rest) = line.strip_prefix(b"event:") {
            event = Some(parse_field_value(rest));
        } else if let Some(rest) = line.strip_prefix(b"data:") {
            data_lines.push(parse_field_value(rest));
        }

        cursor = next_cursor;
    }

    if data_lines.is_empty() {
        return None;
    }

    Some(SseFrame {
        event,
        data: data_lines.join("\n"),
    })
}

fn parse_field_value(raw: &[u8]) -> String {
    let value = raw.strip_prefix(b" ").unwrap_or(raw);
    String::from_utf8_lossy(value).into_owned()
}

enum TailStatus {
    /// The last line is terminated, so every data line in the tail is complete.
    Complete,
    /// The last line is incomplete and the tail has at most one data line.
    MidLine,
    /// The last line is incomplete and the tail has several data lines, so
    /// emitting the completed prefix would forward truncated JSON.
    TruncatedMultiline,
}

fn classify_tail(buf: &[u8]) -> TailStatus {
    if matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
        return TailStatus::Complete;
    }
    if count_data_lines(buf) > 1 {
        return TailStatus::TruncatedMultiline;
    }
    TailStatus::MidLine
}

fn count_data_lines(buf: &[u8]) -> usize {
    let mut count = 0;
    let mut cursor = 0;
    while cursor < buf.len() {
        let line_start = cursor;
        let (line_end, next) = match read_next_line(buf, cursor) {
            Some(bounds) => bounds,
            None => (buf.len(), buf.len()),
        };
        if buf[line_start..line_end].starts_with(b"data:") {
            count += 1;
        }
        cursor = next;
    }
    count
}
