use super::decoder::{read_next_line, MAX_PENDING_BYTES};
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

#[test]
fn finish_recovers_complete_frame_without_trailing_blank_line() {
    let mut decoder = SseFrameDecoder::new();

    assert!(decoder.push(b"data: {\"ok\":true}\n").is_empty());
    let frames = decoder.finish();

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].event, None);
    assert_eq!(frames[0].data, "{\"ok\":true}");
    assert_eq!(decoder.pending_bytes(), 0);
}

#[test]
fn finish_does_not_emit_mid_line_tail() {
    let mut decoder = SseFrameDecoder::new();

    assert!(decoder.push(b"data: {\"ok\":tr").is_empty());
    assert!(decoder.finish().is_empty());
    assert_eq!(decoder.pending_bytes(), 0);
}

#[test]
fn finish_on_drained_decoder_is_a_no_op() {
    let mut decoder = SseFrameDecoder::new();

    assert!(decoder.finish().is_empty());

    let frames = decoder.push(b"data: done\n\n");
    assert_eq!(frames.len(), 1);
    assert!(decoder.finish().is_empty());
    assert_eq!(decoder.pending_bytes(), 0);
}

#[test]
fn finish_discard_until_boundary_is_a_no_op() {
    let mut decoder = SseFrameDecoder::new();

    let malformed = vec![b'x'; MAX_PENDING_BYTES + 1];
    assert!(decoder.push(&malformed).is_empty());
    assert!(decoder.finish().is_empty());
    assert_eq!(decoder.pending_bytes(), 0);
}

#[test]
fn oversized_frame_is_counted_as_dropped() {
    let mut decoder = SseFrameDecoder::new();
    assert_eq!(decoder.dropped_frame_count(), 0);

    let malformed = vec![b'x'; MAX_PENDING_BYTES + 1];
    assert!(decoder.push(&malformed).is_empty());

    assert_eq!(decoder.dropped_frame_count(), 1);
}

#[test]
fn normal_stream_never_drops_a_frame() {
    let mut decoder = SseFrameDecoder::new();

    let frames = decoder.push(b"data: one\n\ndata: two\n\n");

    assert_eq!(frames.len(), 2);
    assert_eq!(decoder.dropped_frame_count(), 0);
}

#[test]
fn resync_tail_is_counted_as_dropped() {
    let mut decoder = SseFrameDecoder::new();

    let malformed = vec![b'x'; MAX_PENDING_BYTES + 1];
    assert!(decoder.push(&malformed).is_empty());
    assert_eq!(decoder.dropped_frame_count(), 1);

    assert!(decoder.push(b"data: tail\n\n").is_empty());
    assert_eq!(decoder.dropped_frame_count(), 2);
}

#[test]
fn resync_waits_for_blank_line_split_across_chunks() {
    let mut decoder = SseFrameDecoder::new();

    let malformed = vec![b'x'; MAX_PENDING_BYTES + 1];
    assert!(decoder.push(&malformed).is_empty());

    // The data line and the first byte of its blank line arrive together;
    // the blank line is completed by the next chunk.
    assert!(decoder.push(b"data: tail\n").is_empty());
    let frames = decoder.push(b"\ndata: recovered\n\n");

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "recovered");
}

#[test]
fn resync_handles_crlf_blank_line() {
    let mut decoder = SseFrameDecoder::new();

    let malformed = vec![b'x'; MAX_PENDING_BYTES + 1];
    assert!(decoder.push(&malformed).is_empty());

    let frames = decoder.push(b"data: tail\r\n\r\ndata: recovered\r\n\r\n");

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "recovered");
}

#[test]
fn resync_handles_crlf_blank_line_split_across_chunks() {
    let mut decoder = SseFrameDecoder::new();

    let malformed = vec![b'x'; MAX_PENDING_BYTES + 1];
    assert!(decoder.push(&malformed).is_empty());

    // The blank line's CR and LF arrive in separate chunks.
    assert!(decoder.push(b"data: tail\r").is_empty());
    assert!(decoder.push(b"\n\r").is_empty());
    let frames = decoder.push(b"\ndata: recovered\n\n");

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "recovered");
}

#[test]
fn resync_does_not_treat_retained_carriage_return_as_blank_line() {
    let mut decoder = SseFrameDecoder::new();

    let mut malformed = vec![b'x'; MAX_PENDING_BYTES];
    malformed.push(b'\r');
    assert!(decoder.push(&malformed).is_empty());

    // The old decoder kept this CR, read the next chunk's first byte as
    // an empty line, and parsed the discarded frame's tail as a frame.
    assert!(decoder.push(b"data: tail\n\n").is_empty());
    let frames = decoder.push(b"data: recovered\n\n");

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "recovered");
}

#[test]
fn resync_resumes_after_crlf_blank_line_split_from_its_cr() {
    let mut decoder = SseFrameDecoder::new();

    // The oversized buffer ends with the CR of an empty line. The next
    // chunk's LF completes that blank-line boundary, so the frame after
    // the discarded one must be delivered.
    let mut malformed = vec![b'x'; MAX_PENDING_BYTES];
    malformed.extend_from_slice(b"\r\n\r");
    assert!(decoder.push(&malformed).is_empty());

    let frames = decoder.push(b"\ndata: recovered\n\n");

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "recovered");
}

#[test]
fn resync_resumes_after_bare_cr_blank_line_at_buffer_end() {
    let mut decoder = SseFrameDecoder::new();

    let mut malformed = vec![b'x'; MAX_PENDING_BYTES];
    malformed.extend_from_slice(b"\r\n\r");
    assert!(decoder.push(&malformed).is_empty());

    let frames = decoder.push(b"data: recovered\n\n");

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "recovered");
}

#[test]
fn finish_recovers_frame_terminated_by_bare_carriage_return() {
    let mut decoder = SseFrameDecoder::new();

    assert!(decoder.push(b"data: {\"ok\":true}\r").is_empty());
    let frames = decoder.finish();

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "{\"ok\":true}");
    assert_eq!(decoder.pending_bytes(), 0);
}

#[test]
fn finish_recovers_multiline_frame_terminated_by_bare_carriage_return() {
    let mut decoder = SseFrameDecoder::new();

    assert!(decoder
        .push(b"event: update\rdata: first\rdata: second\r")
        .is_empty());
    let frames = decoder.finish();

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].event.as_deref(), Some("update"));
    assert_eq!(frames[0].data, "first\nsecond");
}

#[test]
fn finish_drops_multiline_tail_cut_after_first_line() {
    let mut decoder = SseFrameDecoder::new();

    // The first data line is complete, but the second was cut before its
    // terminator. Emitting the completed prefix would forward truncated
    // JSON inside a well-formed SSE event.
    assert!(decoder
        .push(b"data: {\"first\":\ndata: {\"second\":")
        .is_empty());
    assert!(decoder.finish().is_empty());

    assert_eq!(decoder.pending_bytes(), 0);
    assert_eq!(decoder.dropped_frame_count(), 1);
}

#[test]
fn finish_recovers_complete_multiline_tail_without_blank_line() {
    let mut decoder = SseFrameDecoder::new();

    assert!(decoder.push(b"data: first\ndata: second\n").is_empty());
    let frames = decoder.finish();

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "first\nsecond");
    assert_eq!(decoder.dropped_frame_count(), 0);
}
