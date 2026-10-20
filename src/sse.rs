// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::admission::AdmissionPermit;
use crate::ratelimit::RateLimitTracker;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Context for token verification on streaming responses.
pub struct StreamVerifyCtx {
    pub tier_name: String,
    /// Rate-limit state key (`provider,model` route). Successes must reset
    /// the exact key that 429s dirty, which is per model, not per provider.
    pub ratelimit_key: String,
    /// 429 generation snapshotted when this stream's upstream request began.
    /// A clean stream-end only clears route backoff when no newer 429 has
    /// been recorded since (overlapping-request ordering).
    pub ratelimit_generation: u64,
    pub local_estimate: u64,
    pub ratelimit_tracker: Option<Arc<RateLimitTracker>>,
    pub admission_permit: Option<AdmissionPermit>,
    pub rate_limit_info: Option<(Option<u32>, Option<Instant>)>,
    /// Instant when the upstream HTTP response headers were received.
    /// Used to measure TTFT (time from response start to first content token).
    pub stream_start: Instant,
    /// Maximum idle time between useful SSE frames before the stream is failed.
    pub stream_idle_timeout: Duration,
    /// Resolved token pricing for this request's model, when the provider
    /// declares one. Used to attribute estimated dollar cost per streamed
    /// response; ``None`` leaves the tier's cost unpriced.
    pub pricing: Option<crate::config::ModelPricing>,
}

/// Parsed SSE frame with `event` and combined multi-line `data`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

impl SseFrame {
    /// Re-serialize into SSE format.
    pub fn to_sse_string(&self) -> String {
        let mut out = String::new();
        if let Some(event) = &self.event {
            out.push_str("event: ");
            out.push_str(event);
            out.push('\n');
        }

        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        out
    }
}

/// Incremental SSE decoder that accepts arbitrary byte chunks.
#[derive(Debug, Default)]
pub struct SseFrameDecoder {
    buf: Vec<u8>,
    /// Set after an oversized frame was discarded. The decoder then skips
    /// bytes through the next frame boundary so the tail of a malformed
    /// frame is never parsed or forwarded as if it were a new frame.
    discard_until_boundary: bool,
}

/// Maximum bytes retained while waiting for one SSE frame boundary. This
/// matches the first-event peek limit and prevents a provider that emits an
/// unterminated frame from growing router memory until stream idle timeout.
const MAX_PENDING_BYTES: usize = 1024 * 1024;

impl SseFrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push bytes and emit complete frames that end with a blank line.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        if !chunk.is_empty() {
            self.buf.extend_from_slice(chunk);
        }

        if self.discard_until_boundary {
            // Resume only at a frame boundary. A trailing carriage return may
            // be the first half of a CRLF pair, so it is kept for the next
            // chunk instead of being dropped mid-boundary.
            match find_frame_boundary(&self.buf, 0) {
                Some((_, next_start)) => {
                    self.buf.drain(..next_start);
                    self.discard_until_boundary = false;
                }
                None => {
                    let keep_carriage_return = self.buf.last().copied() == Some(b'\r');
                    self.buf.clear();
                    if keep_carriage_return {
                        self.buf.push(b'\r');
                    }
                    return Vec::new();
                }
            }
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
            self.buf.clear();
            self.discard_until_boundary = true;
            tracing::warn!(
                discarded_bytes = discarded,
                limit_bytes = MAX_PENDING_BYTES,
                "Discarding malformed SSE frame with no boundary"
            );
        }

        frames
    }

    pub fn pending_bytes(&self) -> usize {
        self.buf.len()
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

fn read_next_line(buf: &[u8], start: usize) -> Option<(usize, usize)> {
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
            None => (frame_bytes.len(), frame_bytes.len()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_carriage_return_bounds_a_line_without_entering_its_value() {
        assert_eq!(read_next_line(b"first\rsecond", 0), Some((5, 6)));
        assert_eq!(read_next_line(b"first\r", 0), None);
    }

    #[test]
    fn malformed_unterminated_frame_is_bounded() {
        let mut decoder = SseFrameDecoder::new();

        let malformed = vec![b'x'; MAX_PENDING_BYTES + 1];
        assert!(decoder.push(&malformed).is_empty());
        assert_eq!(decoder.pending_bytes(), 0);
    }

    #[test]
    fn oversized_chunk_of_valid_frames_keeps_every_frame() {
        let mut decoder = SseFrameDecoder::new();

        // Many individually small frames delivered in one chunk larger than
        // the pending-frame limit must all survive: the limit bounds one
        // incomplete frame, not a batch of complete ones.
        let frame_count = 65_536;
        let mut chunk = Vec::new();
        for index in 0..frame_count {
            chunk.extend_from_slice(format!("data: frame-{index}\n\n").as_bytes());
        }
        assert!(chunk.len() > MAX_PENDING_BYTES);
        let frames = decoder.push(&chunk);
        assert_eq!(frames.len(), frame_count);
        assert_eq!(frames[0].data, "frame-0");
        assert_eq!(frames[frame_count - 1].data, "frame-65535");
        assert_eq!(decoder.pending_bytes(), 0);
    }

    #[test]
    fn discarded_oversized_frame_does_not_resume_mid_frame() {
        let mut decoder = SseFrameDecoder::new();

        let malformed = vec![b'x'; MAX_PENDING_BYTES + 1];
        assert!(decoder.push(&malformed).is_empty());

        // The tail of the discarded frame must not be parsed as a new frame.
        assert!(decoder.push(b"data: tail\n\n").is_empty());
        assert_eq!(decoder.pending_bytes(), 0);

        // Parsing resumes at the next boundary, and the frame after the
        // discarded one is delivered intact.
        let frames = decoder.push(b"data: recovered\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "recovered");
    }

    #[test]
    fn decodes_split_event_and_data_fields() {
        let mut decoder = SseFrameDecoder::new();

        assert!(decoder.push(b"eve").is_empty());
        assert!(decoder.push(b"nt: message\nda").is_empty());

        let frames = decoder.push(b"ta: {\"ok\":true}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("message"));
        assert_eq!(frames[0].data, "{\"ok\":true}");
    }

    #[test]
    fn emits_when_boundary_arrives_across_chunks() {
        let mut decoder = SseFrameDecoder::new();

        assert!(decoder.push(b"data: hello\n").is_empty());
        let frames = decoder.push(b"\n");

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, None);
        assert_eq!(frames[0].data, "hello");
    }

    #[test]
    fn decodes_multiline_data_across_chunk_boundaries() {
        let mut decoder = SseFrameDecoder::new();

        assert!(decoder.push(b"data: first l").is_empty());
        assert!(decoder.push(b"ine\ndata: second").is_empty());

        let frames = decoder.push(b" line\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "first line\nsecond line");
    }

    #[test]
    fn preserves_non_json_data_payloads() {
        let mut decoder = SseFrameDecoder::new();

        let frames = decoder.push(b"event: error\ndata: upstream timeout: ECONNRESET\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("error"));
        assert_eq!(frames[0].data, "upstream timeout: ECONNRESET");
    }

    #[test]
    fn handles_crlf_and_multiple_frames_in_one_chunk() {
        let mut decoder = SseFrameDecoder::new();

        let frames = decoder.push(b"data: one\r\n\r\ndata: two\r\n\r\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, "one");
        assert_eq!(frames[1].data, "two");
    }

    #[test]
    fn decodes_highly_fragmented_stream() {
        let mut decoder = SseFrameDecoder::new();
        let payload = b"event: update\ndata: 1234\n\n";

        // Feed one byte at a time
        let mut frames = Vec::new();
        for byte in payload {
            frames.extend(decoder.push(&[*byte]));
        }

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("update"));
        assert_eq!(frames[0].data, "1234");
    }
}
