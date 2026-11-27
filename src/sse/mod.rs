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

mod decoder;
#[cfg(test)]
mod tests;

pub use decoder::SseFrameDecoder;

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
