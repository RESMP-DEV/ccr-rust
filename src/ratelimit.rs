// SPDX-License-Identifier: AGPL-3.0-or-later
use lazy_static::lazy_static;
use parking_lot::RwLock;
use prometheus::{register_counter_vec, CounterVec};
use std::collections::HashMap;
use std::time::{Duration, Instant};

lazy_static! {
    static ref RATE_LIMIT_BACKOFFS_TOTAL: CounterVec = register_counter_vec!(
        "ccr_rate_limit_backoffs_total",
        "Total number of rate limit backoffs per tier",
        &["tier"]
    )
    .unwrap();
}

#[derive(Debug, Default)]
pub struct TierRateLimitState {
    pub remaining: Option<u32>,
    pub reset_at: Option<Instant>,
    pub backoff_until: Option<Instant>,
    pub consecutive_429s: u32,
}

#[derive(Default)]
pub struct RateLimitTracker {
    tiers: RwLock<HashMap<String, TierRateLimitState>>,
}

impl RateLimitTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check whether a tier should be skipped before dispatching.
    ///
    /// `honor_remaining` controls whether `X-RateLimit-Remaining: 0` from a
    /// successful response causes proactive skipping.  Set to `false` for
    /// providers that include rate-limit headers on every 200 as informational
    /// warnings (e.g. Z.AI).
    pub fn should_skip_tier(&self, tier: &str, honor_remaining: bool) -> bool {
        let tiers = self.tiers.read();
        if let Some(state) = tiers.get(tier) {
            let now = Instant::now();

            // Always skip when in exponential backoff from a real 429.
            if let Some(until) = state.backoff_until {
                if now < until {
                    tracing::debug!(
                        tier = %tier,
                        backoff_remaining_secs = %until.saturating_duration_since(now).as_secs(),
                        "Skipping tier: backoff in effect"
                    );
                    return true;
                }
            }

            // Proactive skip when remaining quota is exhausted — only when
            // the provider's headers are trusted (honor_ratelimit_headers).
            if let Some(0) = state.remaining {
                if let Some(reset) = state.reset_at {
                    if now < reset {
                        if honor_remaining {
                            tracing::debug!(
                                tier = %tier,
                                reset_remaining_secs = %reset.saturating_duration_since(now).as_secs(),
                                "Skipping tier: quota exhausted"
                            );
                            return true;
                        }
                        tracing::debug!(
                            tier = %tier,
                            reset_remaining_secs = %reset.saturating_duration_since(now).as_secs(),
                            "Informational: quota appears exhausted (not blocking)"
                        );
                    }
                }
            }
        }
        false
    }

    /// Returns true if the tier is in exponential backoff from a real 429.
    pub fn has_backoff(&self, tier: &str) -> bool {
        let tiers = self.tiers.read();
        tiers
            .get(tier)
            .and_then(|state| state.backoff_until)
            .is_some_and(|until| Instant::now() < until)
    }

    pub fn record_429(&self, tier: &str, retry_after: Option<Duration>) {
        let mut tiers = self.tiers.write();
        let state = tiers.entry(tier.to_string()).or_default();
        state.consecutive_429s += 1;

        // Exponential backoff: 1s, 2s, 4s, 8s... capped at 60s
        let base_backoff = retry_after.unwrap_or(Duration::from_secs(1));
        let multiplier = 2u32.saturating_pow(state.consecutive_429s.min(6));
        let backoff = base_backoff
            .saturating_mul(multiplier)
            .min(Duration::from_secs(60));

        state.backoff_until = Some(Instant::now() + backoff);

        tracing::warn!(
            tier = %tier,
            backoff_secs = %backoff.as_secs(),
            consecutive = %state.consecutive_429s,
            retry_after = ?retry_after,
            "Rate limited, backing off"
        );

        // Record to Prometheus metric
        RATE_LIMIT_BACKOFFS_TOTAL.with_label_values(&[tier]).inc();
    }

    pub fn record_success(&self, tier: &str, remaining: Option<u32>, reset_at: Option<Instant>) {
        let mut tiers = self.tiers.write();
        let state = tiers.entry(tier.to_string()).or_default();

        // Clear backoff state on successful request
        state.consecutive_429s = 0;
        state.backoff_until = None;

        // Update rate limit info from response headers
        state.remaining = remaining;
        state.reset_at = reset_at;

        tracing::debug!(
            tier = %tier,
            remaining = ?remaining,
            reset_at = ?reset_at.map(|t| format!("{:?}", t.saturating_duration_since(Instant::now()).as_secs())),
            "Updated rate limit state"
        );
    }
}

/// Restore persisted backoff counters into Prometheus at startup.
pub fn restore_rate_limit_backoff_counter(tier: &str, value: f64) {
    if value > 0.0 {
        RATE_LIMIT_BACKOFFS_TOTAL
            .with_label_values(&[tier])
            .inc_by(value);
    }
}
