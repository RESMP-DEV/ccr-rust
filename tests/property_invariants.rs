// SPDX-License-Identifier: AGPL-3.0-or-later
use ccr_rust::config::TierRetryConfig;
use ccr_rust::sse::{SseFrame, SseFrameDecoder};
use proptest::prelude::*;
use std::time::Duration;

proptest! {
    /// Serialization, arbitrary chunk boundaries, and decoding must preserve a
    /// frame. This is the invariant that lets real networks split SSE bytes at
    /// any position without synthesizing or losing model events.
    #[test]
    fn sse_frames_survive_arbitrary_chunk_boundaries(
        event in "[A-Za-z0-9_.:-]{0,64}",
        // SSE uses bare CR as a line terminator, so a data value containing
        // CR has no byte-preserving serialization in the wire format.
        data in "[^\r]*",
        cut_seeds in prop::collection::vec(any::<u64>(), 0usize..12),
    ) {
        let frame = SseFrame {
            event: if event.is_empty() { None } else { Some(event.clone()) },
            data: data.clone(),
        };
        let wire = frame.to_sse_string();
        let mut boundaries: Vec<usize> = cut_seeds
            .iter()
            .map(|seed| (*seed as usize) % (wire.len() + 1))
            .collect();
        boundaries.push(0);
        boundaries.push(wire.len());
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut decoder = SseFrameDecoder::new();
        let mut frames = Vec::new();
        for window in boundaries.windows(2) {
            frames.extend(decoder.push(&wire.as_bytes()[window[0]..window[1]]));
        }

        prop_assert_eq!(frames, vec![frame]);
    }

    /// Retry delays must stay bounded, and a multiplier of at least one must
    /// never make a later retry wait less than an earlier retry after the
    /// configured ceiling is taken into account.
    #[test]
    fn retry_backoff_is_bounded_and_non_decreasing(
        base_ms in 1u64..20_000,
        multiplier in 1.0f64..4.0,
        max_ms in 1u64..60_000,
        attempt in 0usize..24,
    ) {
        let config = TierRetryConfig {
            base_backoff_ms: base_ms,
            backoff_multiplier: multiplier,
            max_backoff_ms: max_ms,
            ..Default::default()
        };

        let delay = config.backoff_duration(attempt);
        prop_assert!(delay <= Duration::from_millis(max_ms));

        if attempt > 0 {
            let previous = config.backoff_duration(attempt - 1);
            prop_assert!(delay >= previous);
        }
    }
}

#[test]
fn complete_frame_drains_but_partial_tail_remains_buffered() {
    let mut decoder = SseFrameDecoder::new();

    let complete = decoder.push(b"data: done\n\ndata: par");
    assert_eq!(
        complete,
        vec![SseFrame {
            event: None,
            data: "done".to_owned(),
        }]
    );

    let tail = decoder.push(b"tial\n\n");
    assert_eq!(
        tail,
        vec![SseFrame {
            event: None,
            data: "partial".to_owned(),
        }]
    );
}

#[test]
fn crlf_line_endings_survive_a_split_carriage_return() {
    let mut decoder = SseFrameDecoder::new();

    assert!(decoder.push(b"event: message\r\ndata: first\r").is_empty());
    let frames = decoder.push(b"\n\n");

    assert_eq!(
        frames,
        vec![SseFrame {
            event: Some("message".to_owned()),
            data: "first".to_owned(),
        }]
    );
}
