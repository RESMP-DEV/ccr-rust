use parking_lot::RwLock;
use std::collections::HashMap;
use std::time::{Duration, Instant};

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

    pub fn should_skip_tier(&self, tier: &str) -> bool {
        let tiers = self.tiers.read();
        if let Some(state) = tiers.get(tier) {
            if let Some(until) = state.backoff_until {
                if Instant::now() < until {
                    return true;
                }
            }
        }
        false
    }

    pub fn record_429(&self, tier: &str, retry_after: Option<Duration>) {
        let mut tiers = self.tiers.write();
        let state = tiers.entry(tier.to_string()).or_default();
        state.consecutive_429s += 1;

        // Exponential backoff: 1s, 2s, 4s, 8s... capped at 60s
        let base_backoff = retry_after.unwrap_or(Duration::from_secs(1));
        let multiplier = 2u32.saturating_pow(state.consecutive_429s.min(6));
        let backoff = base_backoff.saturating_mul(multiplier).min(Duration::from_secs(60));

        state.backoff_until = Some(Instant::now() + backoff);

        tracing::warn!(
            tier = %tier,
            backoff_secs = %backoff.as_secs(),
            consecutive = %state.consecutive_429s,
            "Rate limited, backing off"
        );
    }

    pub fn record_success(&self, tier: &str) {
        let mut tiers = self.tiers.write();
        if let Some(state) = tiers.get_mut(tier) {
            state.consecutive_429s = 0;
            state.backoff_until = None;
        }
    }
}
