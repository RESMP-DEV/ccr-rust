use crate::routing::EwmaTracker;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, trace};

/// Dynamic backoff scaler that adjusts retry delays based on EWMA latency trends.
///
/// This scaler uses the ratio of a tier's current EWMA to a reference baseline,
/// allowing faster tiers to retry more aggressively while slowing down retries
/// for degraded or slow tiers.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DynamicBackoff {
    /// Reference latency in milliseconds (default: 1000ms = 1 second).
    /// Tiers with EWMA below this scale down their backoff; above scales up.
    reference_latency_ms: f64,
    /// Minimum scaling factor (default: 0.5). Prevents overly aggressive retries.
    min_scale: f64,
    /// Maximum scaling factor (default: 3.0). Prevents excessive delays.
    max_scale: f64,
    /// EWMA tracker for retrieving current tier latencies.
    ewma_tracker: Arc<EwmaTracker>,
}

#[allow(dead_code)]
impl DynamicBackoff {
    /// Create a new dynamic backoff scaler with default parameters.
    pub fn new(ewma_tracker: Arc<EwmaTracker>) -> Self {
        Self {
            reference_latency_ms: 1000.0,
            min_scale: 0.5,
            max_scale: 3.0,
            ewma_tracker,
        }
    }

    /// Create a new dynamic backoff scaler with custom parameters.
    ///
    /// # Arguments
    /// * `ewma_tracker` - The EWMA latency tracker for tier performance data.
    /// * `reference_latency_ms` - Baseline latency in milliseconds for scaling comparison.
    /// * `min_scale` - Minimum backoff multiplier (e.g., 0.5 = half the base delay).
    /// * `max_scale` - Maximum backoff multiplier (e.g., 3.0 = triple the base delay).
    pub fn with_params(
        ewma_tracker: Arc<EwmaTracker>,
        reference_latency_ms: f64,
        min_scale: f64,
        max_scale: f64,
    ) -> Self {
        Self {
            reference_latency_ms: reference_latency_ms.max(1.0),
            min_scale: min_scale.clamp(0.1, 10.0),
            max_scale: max_scale.clamp(0.1, 10.0),
            ewma_tracker,
        }
    }

    /// Calculate the dynamic scaling factor for a tier based on its EWMA latency.
    ///
    /// The scaling factor is computed as:
    /// ```text
    /// factor = clamp(ewma_ms / reference_ms, min_scale, max_scale)
    /// ```
    ///
    /// Returns 1.0 (no scaling) if the tier has no EWMA data.
    pub fn scale_factor(&self, tier_name: &str) -> f64 {
        match self.ewma_tracker.get_latency(tier_name) {
            Some((ewma_secs, _samples)) if ewma_secs > 0.0 => {
                let ewma_ms = ewma_secs * 1000.0;
                let factor =
                    (ewma_ms / self.reference_latency_ms).clamp(self.min_scale, self.max_scale);
                trace!(
                    tier = tier_name,
                    ewma_ms = ewma_ms,
                    reference_ms = self.reference_latency_ms,
                    factor = factor,
                    "calculated dynamic backoff scale factor"
                );
                factor
            }
            _ => {
                trace!(
                    tier = tier_name,
                    factor = 1.0,
                    "no EWMA data, using neutral scale factor"
                );
                1.0
            }
        }
    }

    /// Calculate a dynamically-scaled backoff duration for a tier.
    ///
    /// # Arguments
    /// * `tier_name` - The tier to look up EWMA for.
    /// * `base_duration` - The base backoff duration before scaling.
    ///
    /// # Returns
    /// The scaled duration, clamped to ensure it's at least 1ms.
    pub fn scale_backoff(&self, tier_name: &str, base_duration: Duration) -> Duration {
        let factor = self.scale_factor(tier_name);
        let base_ms = base_duration.as_millis() as f64;
        let scaled_ms = (base_ms * factor).max(1.0) as u64;

        let scaled = Duration::from_millis(scaled_ms);
        debug!(
            tier = tier_name,
            base_ms = base_ms,
            scaled_ms = scaled_ms,
            factor = factor,
            "applied dynamic backoff scaling"
        );
        scaled
    }

    /// Calculate exponential backoff with dynamic EWMA scaling.
    ///
    /// This combines exponential backoff (base * 2^attempt) with the EWMA-based
    /// scaling factor for the tier.
    ///
    /// # Arguments
    /// * `tier_name` - The tier to look up EWMA for.
    /// * `base_ms` - Base backoff in milliseconds.
    /// * `attempt` - Retry attempt index (0-indexed).
    /// * `max_ms` - Maximum backoff cap in milliseconds.
    ///
    /// # Returns
    /// The computed backoff duration with EWMA scaling applied.
    pub fn exponential_with_ewma(
        &self,
        tier_name: &str,
        base_ms: u64,
        attempt: usize,
        max_ms: u64,
    ) -> Duration {
        // Calculate base exponential backoff
        let exponential_ms = base_ms.saturating_mul(2_u64.saturating_pow(attempt as u32));
        let clamped_base = exponential_ms.min(max_ms);

        // Apply EWMA scaling
        let factor = self.scale_factor(tier_name);
        let scaled_ms = (clamped_base as f64 * factor).max(1.0) as u64;
        let final_ms = scaled_ms.min(max_ms).max(base_ms);

        debug!(
            tier = tier_name,
            attempt = attempt,
            base_ms = base_ms,
            exponential_ms = clamped_base,
            factor = factor,
            scaled_ms = scaled_ms,
            final_ms = final_ms,
            "calculated EWMA-scaled exponential backoff"
        );

        Duration::from_millis(final_ms)
    }

    /// Get the reference latency in milliseconds.
    pub fn reference_latency_ms(&self) -> f64 {
        self.reference_latency_ms
    }

    /// Get the minimum scaling factor.
    pub fn min_scale(&self) -> f64 {
        self.min_scale
    }

    /// Get the maximum scaling factor.
    pub fn max_scale(&self) -> f64 {
        self.max_scale
    }

    /// Update the reference latency baseline.
    pub fn set_reference_latency_ms(&mut self, ms: f64) {
        self.reference_latency_ms = ms.max(1.0);
    }
}

/// Convenience function to create a scaled backoff duration.
///
/// This is a stateless version that uses an explicit EWMA value instead of
/// looking up from a tracker.
///
/// # Arguments
/// * `base_duration` - The base backoff duration.
/// * `ewma_secs` - Current EWMA latency in seconds, if available.
/// * `reference_ms` - Reference latency baseline in milliseconds.
/// * `min_scale` - Minimum scaling factor.
/// * `max_scale` - Maximum scaling factor.
///
/// # Returns
/// The scaled duration. If `ewma_secs` is None, returns `base_duration` unchanged.
#[allow(dead_code)]
pub fn scale_backoff_with_ewma(
    base_duration: Duration,
    ewma_secs: Option<f64>,
    reference_ms: f64,
    min_scale: f64,
    max_scale: f64,
) -> Duration {
    match ewma_secs {
        Some(ewma) if ewma > 0.0 => {
            let ewma_ms = ewma * 1000.0;
            let factor = (ewma_ms / reference_ms).clamp(min_scale, max_scale);
            let base_ms = base_duration.as_millis() as f64;
            let scaled_ms = (base_ms * factor).max(1.0) as u64;
            Duration::from_millis(scaled_ms)
        }
        _ => base_duration,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_tracker_with_latency(tier: &str, latency_secs: f64) -> Arc<EwmaTracker> {
        let tracker = Arc::new(EwmaTracker::new());
        tracker.record_success(tier, latency_secs);
        tracker
    }

    #[test]
    fn test_neutral_scaling_when_no_ewma() {
        let tracker = Arc::new(EwmaTracker::new());
        let backoff = DynamicBackoff::new(tracker);

        // No EWMA recorded for tier-0, should return 1.0
        assert_eq!(backoff.scale_factor("tier-0"), 1.0);
    }

    #[test]
    fn test_fast_tier_scales_down() {
        // 300ms latency tier (fast)
        let tracker = create_tracker_with_latency("tier-0", 0.3);
        let backoff = DynamicBackoff::new(tracker);

        // 300ms / 1000ms = 0.3, clamped to min 0.5
        assert_eq!(backoff.scale_factor("tier-0"), 0.5);
    }

    #[test]
    fn test_normal_tier_no_scaling() {
        // 1000ms latency tier (reference baseline)
        let tracker = create_tracker_with_latency("tier-0", 1.0);
        let backoff = DynamicBackoff::new(tracker);

        // 1000ms / 1000ms = 1.0
        assert!((backoff.scale_factor("tier-0") - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_slow_tier_scales_up() {
        // 2500ms latency tier (slow)
        let tracker = create_tracker_with_latency("tier-0", 2.5);
        let backoff = DynamicBackoff::new(tracker);

        // 2500ms / 1000ms = 2.5
        assert!((backoff.scale_factor("tier-0") - 2.5).abs() < 0.001);
    }

    #[test]
    fn test_very_slow_tier_clamped() {
        // 5000ms latency tier (very slow)
        let tracker = create_tracker_with_latency("tier-0", 5.0);
        let backoff = DynamicBackoff::new(tracker);

        // 5000ms / 1000ms = 5.0, clamped to max 3.0
        assert_eq!(backoff.scale_factor("tier-0"), 3.0);
    }

    #[test]
    fn test_scale_backoff_applies_factor() {
        let tracker = create_tracker_with_latency("tier-0", 0.5); // Fast tier
        let backoff = DynamicBackoff::new(tracker);

        let base = Duration::from_millis(100);
        let scaled = backoff.scale_backoff("tier-0", base);

        // 100ms * 0.5 = 50ms
        assert_eq!(scaled, Duration::from_millis(50));
    }

    #[test]
    fn test_exponential_with_ewma() {
        let tracker = create_tracker_with_latency("tier-0", 2.0); // 2x slower than baseline
        let backoff = DynamicBackoff::new(tracker);

        // Attempt 0: base 100ms * 2^0 = 100ms, scaled by 2.0 = 200ms
        let d0 = backoff.exponential_with_ewma("tier-0", 100, 0, 10000);
        assert_eq!(d0, Duration::from_millis(200));

        // Attempt 1: base 100ms * 2^1 = 200ms, scaled by 2.0 = 400ms
        let d1 = backoff.exponential_with_ewma("tier-0", 100, 1, 10000);
        assert_eq!(d1, Duration::from_millis(400));

        // Attempt 2: base 100ms * 2^2 = 400ms, scaled by 2.0 = 800ms
        let d2 = backoff.exponential_with_ewma("tier-0", 100, 2, 10000);
        assert_eq!(d2, Duration::from_millis(800));
    }

    #[test]
    fn test_exponential_respects_max_cap() {
        let tracker = create_tracker_with_latency("tier-0", 3.0); // 3x scaling
        let backoff = DynamicBackoff::new(tracker);

        // Attempt 2: base 100 * 4 = 400, scaled by 3 = 1200, but max is 500
        let d2 = backoff.exponential_with_ewma("tier-0", 100, 2, 500);
        assert_eq!(d2, Duration::from_millis(500));
    }

    #[test]
    fn test_exponential_respects_min_base() {
        let tracker = create_tracker_with_latency("tier-0", 0.5); // 0.5x scaling
        let backoff = DynamicBackoff::new(tracker);

        // Should not go below base_backoff_ms even with small scaling
        let d0 = backoff.exponential_with_ewma("tier-0", 100, 0, 10000);
        assert_eq!(d0, Duration::from_millis(100)); // 100 * 0.5 = 50, but clamped to 100
    }

    #[test]
    fn test_custom_params() {
        let tracker = create_tracker_with_latency("tier-0", 2.0);
        let backoff = DynamicBackoff::with_params(
            tracker, 500.0, // 500ms reference (faster baseline)
            0.25,  // min 0.25x
            5.0,   // max 5.0x
        );

        // 2000ms / 500ms = 4.0, within custom bounds
        assert!((backoff.scale_factor("tier-0") - 4.0).abs() < 0.001);
    }

    #[test]
    fn test_stateless_scale_function() {
        let base = Duration::from_millis(100);

        // Fast tier: 300ms EWMA
        let scaled_fast = scale_backoff_with_ewma(base, Some(0.3), 1000.0, 0.5, 3.0);
        assert_eq!(scaled_fast, Duration::from_millis(50)); // 100 * 0.5

        // Slow tier: 2500ms EWMA
        let scaled_slow = scale_backoff_with_ewma(base, Some(2.5), 1000.0, 0.5, 3.0);
        assert_eq!(scaled_slow, Duration::from_millis(250)); // 100 * 2.5

        // No EWMA data
        let scaled_none = scale_backoff_with_ewma(base, None, 1000.0, 0.5, 3.0);
        assert_eq!(scaled_none, base); // unchanged
    }

    #[test]
    fn test_zero_ewma_treated_as_no_data() {
        let tracker = Arc::new(EwmaTracker::new());
        // Manually record with zero (should not happen in practice, but test defense)
        tracker.record_success("tier-0", 0.0);

        let backoff = DynamicBackoff::new(tracker);
        // Zero EWMA should be treated as "no data" and return 1.0
        assert_eq!(backoff.scale_factor("tier-0"), 1.0);
    }

    #[test]
    fn test_reference_latency_getter() {
        let tracker = Arc::new(EwmaTracker::new());
        let backoff = DynamicBackoff::with_params(tracker, 2000.0, 0.3, 4.0);

        assert_eq!(backoff.reference_latency_ms(), 2000.0);
        assert_eq!(backoff.min_scale(), 0.3);
        assert_eq!(backoff.max_scale(), 4.0);
    }

    #[test]
    fn test_set_reference_latency() {
        let tracker = Arc::new(EwmaTracker::new());
        let mut backoff = DynamicBackoff::new(tracker);

        backoff.set_reference_latency_ms(500.0);
        assert_eq!(backoff.reference_latency_ms(), 500.0);

        // Should clamp to at least 1.0
        backoff.set_reference_latency_ms(0.5);
        assert_eq!(backoff.reference_latency_ms(), 1.0);
    }
}
