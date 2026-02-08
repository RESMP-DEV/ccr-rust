use parking_lot::RwLock;
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info};

/// EWMA smoothing factor. 0.3 = 30% weight on new sample, 70% on history.
/// Higher values react faster to latency changes but are noisier.
const DEFAULT_EWMA_ALPHA: f64 = 0.3;

/// Minimum samples before a tier's EWMA is trusted for routing decisions.
/// Below this threshold, tiers keep their config-defined priority order.
const DEFAULT_MIN_SAMPLES: u64 = 3;

/// Penalty multiplier applied to EWMA when a request fails.
/// A failed request is treated as if it took (current_ewma * penalty) seconds,
/// pushing the tier down in priority without catastrophically inflating the estimate.
const DEFAULT_FAILURE_PENALTY: f64 = 2.0;

/// Per-tier latency tracking state.
#[derive(Debug, Clone)]
struct TierState {
    /// Current EWMA latency in seconds.
    ewma: f64,
    /// Total number of samples (successes + failures) recorded.
    samples: u64,
    /// Number of consecutive failures (resets on success).
    consecutive_failures: u64,
}

impl TierState {
    fn new() -> Self {
        Self {
            ewma: 0.0,
            samples: 0,
            consecutive_failures: 0,
        }
    }
}

/// EWMA-based latency tracker for backend tier routing.
///
/// Tracks per-attempt latency (not total request duration across retries) so
/// that the EWMA reflects actual backend responsiveness. Failed requests apply
/// a penalty multiplier to the current EWMA rather than recording wall-clock
/// time, since failure latency (timeouts, connection refused) doesn't correlate
/// with backend speed.
#[derive(Debug)]
pub struct EwmaTracker {
    state: RwLock<HashMap<String, TierState>>,
    alpha: f64,
    min_samples: u64,
    failure_penalty: f64,
}

impl Default for EwmaTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl EwmaTracker {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(HashMap::new()),
            alpha: DEFAULT_EWMA_ALPHA,
            min_samples: DEFAULT_MIN_SAMPLES,
            failure_penalty: DEFAULT_FAILURE_PENALTY,
        }
    }

    /// Create a tracker with custom parameters.
    #[allow(dead_code)]
    pub fn with_params(alpha: f64, min_samples: u64, failure_penalty: f64) -> Self {
        Self {
            state: RwLock::new(HashMap::new()),
            alpha: alpha.clamp(0.01, 1.0),
            min_samples: min_samples.max(1),
            failure_penalty: failure_penalty.max(1.0),
        }
    }

    /// Record a successful request's latency for a tier.
    pub fn record_success(&self, tier: &str, duration_secs: f64) {
        let mut state = self.state.write();
        let entry = state.entry(tier.to_string()).or_insert_with(TierState::new);

        if entry.samples == 0 {
            entry.ewma = duration_secs;
        } else {
            entry.ewma = self.alpha * duration_secs + (1.0 - self.alpha) * entry.ewma;
        }
        entry.samples += 1;
        entry.consecutive_failures = 0;

        debug!(
            tier = tier,
            ewma = entry.ewma,
            samples = entry.samples,
            "EWMA updated (success)"
        );
    }

    /// Record a failed request for a tier. Applies a penalty to the EWMA
    /// without requiring a wall-clock duration (failures often hit timeouts
    /// that don't reflect backend speed).
    pub fn record_failure(&self, tier: &str) {
        let mut state = self.state.write();
        let entry = state.entry(tier.to_string()).or_insert_with(TierState::new);

        entry.consecutive_failures += 1;
        entry.samples += 1;

        // Only penalize if we have a baseline EWMA to work from.
        // Otherwise we'd be multiplying zero.
        if entry.ewma > 0.0 {
            let penalty_duration = entry.ewma * self.failure_penalty;
            entry.ewma = self.alpha * penalty_duration + (1.0 - self.alpha) * entry.ewma;
        }

        debug!(
            tier = tier,
            ewma = entry.ewma,
            consecutive_failures = entry.consecutive_failures,
            "EWMA updated (failure penalty)"
        );
    }

    /// Get the current EWMA latency for a specific tier.
    /// Returns `None` if the tier has no recorded samples.
    pub fn get_latency(&self, tier: &str) -> Option<(f64, u64)> {
        let state = self.state.read();
        state.get(tier).map(|s| (s.ewma, s.samples))
    }

    /// Get latencies for all tracked tiers.
    /// Returns `(tier_name, ewma_seconds, sample_count)` tuples.
    pub fn get_all_latencies(&self) -> Vec<(String, f64, u64)> {
        let state = self.state.read();
        state
            .iter()
            .map(|(k, v)| (k.clone(), v.ewma, v.samples))
            .collect()
    }

    /// Restore a tier EWMA snapshot, used by persistence backends at startup.
    pub fn restore_tier_state(&self, tier: &str, ewma: f64, samples: u64) {
        let mut state = self.state.write();
        let entry = state.entry(tier.to_string()).or_insert_with(TierState::new);
        entry.ewma = ewma.max(0.0);
        entry.samples = samples;
        entry.consecutive_failures = 0;
    }

    /// Reorder tiers by EWMA latency (lowest first). Tiers without enough
    /// samples keep their config-defined position.
    ///
    /// Returns `(tier_route, tier_name)` pairs in priority order.
    pub fn sort_tiers(&self, tiers: &[String]) -> Vec<(String, String)> {
        let state = self.state.read();

        let mut entries: Vec<(usize, String, String, Option<f64>)> = tiers
            .iter()
            .enumerate()
            .map(|(idx, tier)| {
                let tier_name = crate::config::Config::backend_abbreviation(tier);
                let ewma = state.get(&tier_name).and_then(|s| {
                    if s.samples >= self.min_samples {
                        Some(s.ewma)
                    } else {
                        None
                    }
                });
                (idx, tier.clone(), tier_name, ewma)
            })
            .collect();

        // Stable sort: measured tiers by EWMA ascending, unmeasured keep config order.
        entries.sort_by(|a, b| match (a.3, b.3) {
            (Some(la), Some(lb)) => la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0),
        });

        if tracing::enabled!(tracing::Level::DEBUG) {
            let order: Vec<String> = entries
                .iter()
                .map(|(_, _, name, ewma)| match ewma {
                    Some(e) => format!("{}({:.3}s)", name, e),
                    None => format!("{}(?)", name),
                })
                .collect();
            debug!(order = ?order, "tier routing order");
        }

        entries
            .into_iter()
            .map(|(_, tier, name, _)| (tier, name))
            .collect()
    }

    /// Sort tiers with config-aware tier name resolution.
    ///
    /// Uses `tier_name` from provider config when available.
    pub fn sort_tiers_with_config(
        &self,
        tiers: &[String],
        config: &crate::config::Config,
    ) -> Vec<(String, String)> {
        let state = self.state.read();

        let mut entries: Vec<(usize, String, String, Option<f64>)> = tiers
            .iter()
            .enumerate()
            .map(|(idx, tier)| {
                let tier_name = config.backend_abbreviation_with_config(tier);
                let ewma = state.get(&tier_name).and_then(|s| {
                    if s.samples >= self.min_samples {
                        Some(s.ewma)
                    } else {
                        None
                    }
                });
                (idx, tier.clone(), tier_name, ewma)
            })
            .collect();

        entries.sort_by(|a, b| match (a.3, b.3) {
            (Some(la), Some(lb)) => la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0),
        });

        if tracing::enabled!(tracing::Level::DEBUG) {
            let order: Vec<String> = entries
                .iter()
                .map(|(_, _, name, ewma)| match ewma {
                    Some(e) => format!("{}({:.3}s)", name, e),
                    None => format!("{}(?)", name),
                })
                .collect();
            debug!(order = ?order, "tier routing order (config-aware)");
        }

        entries
            .into_iter()
            .map(|(_, tier, name, _)| (tier, name))
            .collect()
    }
}

/// Scoped timer for measuring per-attempt latency.
/// Drop it or call `.finish_success()` / `.finish_failure()` to record.
pub struct AttemptTimer<'a> {
    tracker: &'a EwmaTracker,
    tier: String,
    start: Instant,
    recorded: bool,
}

impl<'a> AttemptTimer<'a> {
    pub fn start(tracker: &'a EwmaTracker, tier: &str) -> Self {
        Self {
            tracker,
            tier: tier.to_string(),
            start: Instant::now(),
            recorded: false,
        }
    }

    /// Record a successful attempt. Returns the measured duration in seconds.
    pub fn finish_success(mut self) -> f64 {
        let duration = self.start.elapsed().as_secs_f64();
        self.tracker.record_success(&self.tier, duration);
        self.recorded = true;
        duration
    }

    /// Record a failed attempt. Applies the failure penalty to the EWMA
    /// instead of using the elapsed time.
    pub fn finish_failure(mut self) {
        self.tracker.record_failure(&self.tier);
        self.recorded = true;
    }

    /// Get elapsed time without recording. Useful for logging.
    #[allow(dead_code)]
    pub fn elapsed_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

impl Drop for AttemptTimer<'_> {
    fn drop(&mut self) {
        if !self.recorded {
            // Timer dropped without explicit success/failure. Treat as failure
            // since the caller didn't reach the success path.
            info!(
                tier = self.tier,
                elapsed = self.start.elapsed().as_secs_f64(),
                "AttemptTimer dropped without recording, treating as failure"
            );
            self.tracker.record_failure(&self.tier);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ewma_first_sample() {
        let tracker = EwmaTracker::new();
        tracker.record_success("tier-0", 1.0);
        let (ewma, count) = tracker.get_latency("tier-0").unwrap();
        assert_eq!(ewma, 1.0);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_ewma_convergence() {
        let tracker = EwmaTracker::new();
        // Record 10 samples of 1.0s, then switch to 2.0s
        for _ in 0..10 {
            tracker.record_success("tier-0", 1.0);
        }
        let (ewma_before, _) = tracker.get_latency("tier-0").unwrap();
        assert!((ewma_before - 1.0).abs() < 0.01);

        // Record 10 samples of 2.0s; EWMA should converge toward 2.0
        for _ in 0..10 {
            tracker.record_success("tier-0", 2.0);
        }
        let (ewma_after, _) = tracker.get_latency("tier-0").unwrap();
        assert!(
            ewma_after > 1.5,
            "EWMA should approach 2.0, got {}",
            ewma_after
        );
        assert!(
            ewma_after < 2.0,
            "EWMA should not exceed 2.0, got {}",
            ewma_after
        );
    }

    #[test]
    fn test_failure_penalty() {
        let tracker = EwmaTracker::new();
        tracker.record_success("tier-0", 1.0);
        let (baseline, _) = tracker.get_latency("tier-0").unwrap();

        tracker.record_failure("tier-0");
        let (after_failure, _) = tracker.get_latency("tier-0").unwrap();
        assert!(
            after_failure > baseline,
            "EWMA should increase after failure: {} > {}",
            after_failure,
            baseline
        );
    }

    #[test]
    fn test_failure_penalty_resets_on_success() {
        let tracker = EwmaTracker::new();
        for _ in 0..5 {
            tracker.record_success("tier-0", 1.0);
        }
        // Fail a few times
        tracker.record_failure("tier-0");
        tracker.record_failure("tier-0");
        let (penalized, _) = tracker.get_latency("tier-0").unwrap();

        // Success should bring it back down
        for _ in 0..10 {
            tracker.record_success("tier-0", 1.0);
        }
        let (recovered, _) = tracker.get_latency("tier-0").unwrap();
        assert!(
            recovered < penalized,
            "EWMA should recover after successes: {} < {}",
            recovered,
            penalized
        );
    }

    #[test]
    fn test_sort_tiers_by_latency() {
        let tracker = EwmaTracker::with_params(0.3, 1, 2.0); // min_samples=1 for test
                                                             // provider-a is slow (5s), provider-b is fast (0.5s)
                                                             // Now tier names default to provider name (no hardcoded mapping)
        tracker.record_success("provider-a", 5.0);
        tracker.record_success("provider-b", 0.5);

        let tiers = vec![
            "provider-a,model-a".to_string(),
            "provider-b,model-b".to_string(),
        ];
        let sorted = tracker.sort_tiers(&tiers);

        assert_eq!(sorted[0].1, "provider-b", "faster tier should come first");
        assert_eq!(sorted[1].1, "provider-a", "slower tier should come second");
    }

    #[test]
    fn test_unmeasured_tiers_keep_config_order() {
        let tracker = EwmaTracker::new(); // min_samples=3
                                          // Only 1 sample for "a" (below threshold), none for others
                                          // Simple tier names without comma return as-is
        tracker.record_success("a", 1.0);

        let tiers = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let sorted = tracker.sort_tiers(&tiers);

        // No tier has enough samples, so config order is preserved
        // Simple tier names (no comma) return as-is
        assert_eq!(sorted[0].1, "a");
        assert_eq!(sorted[1].1, "b");
        assert_eq!(sorted[2].1, "c");
    }

    #[test]
    fn test_custom_alpha() {
        // High alpha (0.9) should converge much faster
        let tracker = EwmaTracker::with_params(0.9, 1, 2.0);
        tracker.record_success("tier-0", 1.0);
        tracker.record_success("tier-0", 10.0);
        let (ewma, _) = tracker.get_latency("tier-0").unwrap();
        // With alpha=0.9: ewma = 0.9*10 + 0.1*1 = 9.1
        assert!((ewma - 9.1).abs() < 0.01, "expected ~9.1, got {}", ewma);
    }

    #[test]
    fn test_attempt_timer_success() {
        let tracker = EwmaTracker::new();
        let timer = AttemptTimer::start(&tracker, "tier-0");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let duration = timer.finish_success();
        assert!(duration >= 0.01, "duration should be >= 10ms");

        let (ewma, count) = tracker.get_latency("tier-0").unwrap();
        assert!(ewma > 0.0);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_attempt_timer_failure() {
        let tracker = EwmaTracker::new();
        // Give it a baseline first
        tracker.record_success("tier-0", 1.0);

        let timer = AttemptTimer::start(&tracker, "tier-0");
        timer.finish_failure();

        let (ewma, count) = tracker.get_latency("tier-0").unwrap();
        assert!(ewma > 1.0, "failure penalty should increase EWMA");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_attempt_timer_drop_records_failure() {
        let tracker = EwmaTracker::new();
        tracker.record_success("tier-0", 1.0);

        {
            let _timer = AttemptTimer::start(&tracker, "tier-0");
            // Dropped without calling finish_success or finish_failure
        }

        let (_, count) = tracker.get_latency("tier-0").unwrap();
        assert_eq!(count, 2, "drop should have recorded a failure");
    }

    #[test]
    fn test_get_all_latencies() {
        let tracker = EwmaTracker::new();
        tracker.record_success("tier-0", 1.0);
        tracker.record_success("tier-1", 2.0);
        tracker.record_success("tier-2", 0.5);

        let latencies = tracker.get_all_latencies();
        assert_eq!(latencies.len(), 3);
    }

    #[test]
    fn test_no_latency_for_unknown_tier() {
        let tracker = EwmaTracker::new();
        assert!(tracker.get_latency("nonexistent").is_none());
    }
}
