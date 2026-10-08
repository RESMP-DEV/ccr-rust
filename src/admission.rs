// SPDX-License-Identifier: AGPL-3.0-or-later
//! Adaptive per-tier in-flight admission control.
//!
//! A no-guidance 429 often means the plan is admitting some requests while
//! rejecting others. A global backoff window over-corrects by idling a tier
//! that could still make progress. AIMD admission limits concurrency locally:
//! a rejection halves the tier limit, while a completed success grows it by
//! one bounded step.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::metrics;

const DEFAULT_INITIAL_LIMIT: usize = 8;
const DEFAULT_MIN_LIMIT: usize = 1;
const DEFAULT_MAX_LIMIT: usize = 64;

#[derive(Debug)]
struct TierAdmission {
    inflight: AtomicU64,
    limit: AtomicU64,
    ceiling: AtomicU64,
}

impl TierAdmission {
    /// Resolve one route's AIMD bounds. An explicit `maxInflight` pins both
    /// the starting limit and the ceiling; an unconfigured route starts at
    /// `DEFAULT_INITIAL_LIMIT` and may grow up to `DEFAULT_MAX_LIMIT`.
    fn bounds(provider_limit: Option<usize>) -> (u64, u64) {
        let clamp = |value: usize| value.clamp(DEFAULT_MIN_LIMIT, DEFAULT_MAX_LIMIT) as u64;
        let ceiling = clamp(provider_limit.unwrap_or(DEFAULT_MAX_LIMIT));
        let initial = clamp(provider_limit.unwrap_or(DEFAULT_INITIAL_LIMIT)).min(ceiling);
        (initial, ceiling)
    }

    fn new(provider_limit: Option<usize>) -> Self {
        let (initial, ceiling) = Self::bounds(provider_limit);
        Self {
            inflight: AtomicU64::new(0),
            limit: AtomicU64::new(initial),
            ceiling: AtomicU64::new(ceiling),
        }
    }

    fn limit(&self) -> u64 {
        self.limit.load(Ordering::Acquire)
    }

    /// Re-apply a configured ceiling. Raising it never lifts an existing AIMD
    /// cut; lowering it clamps the live limit immediately so an operator
    /// reduction is honored without waiting for traffic.
    fn set_ceiling(&self, provider_limit: Option<usize>) {
        let (_, ceiling) = Self::bounds(provider_limit);
        self.ceiling.store(ceiling, Ordering::Release);
        let _ = self
            .limit
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let capped = current.min(ceiling);
                (capped != current).then_some(capped)
            });
    }

    /// Additive increase. The clamp is part of the same atomic transition as
    /// the increment, so concurrent successes cannot lose a step and no
    /// observer can see a limit above the ceiling.
    fn record_success(&self, key: &str) {
        let grown = self
            .limit
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let ceiling = self.ceiling.load(Ordering::Acquire);
                Some(current.saturating_add(1).min(ceiling))
            })
            .map_or(DEFAULT_MAX_LIMIT as u64, |old| {
                old.saturating_add(1)
                    .min(self.ceiling.load(Ordering::Acquire))
            });
        metrics::record_tier_limit(key, grown);
    }

    /// Multiplicative decrease. Halving is one atomic transition, so
    /// concurrent rejections compose and a rejection can never be undone by
    /// a concurrent success that read the limit before the cut.
    fn record_rejection(&self, key: &str) {
        let cut = self
            .limit
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some((current / 2).max(DEFAULT_MIN_LIMIT as u64))
            })
            .map_or(DEFAULT_MIN_LIMIT as u64, |old| {
                (old / 2).max(DEFAULT_MIN_LIMIT as u64)
            });
        metrics::record_tier_limit(key, cut);
    }
}

/// Shared AIMD admission tracker keyed by full `provider,model` route.
#[derive(Debug, Default)]
pub struct AdmissionTracker {
    tiers: Mutex<HashMap<String, Arc<TierAdmission>>>,
}

/// RAII in-flight slot. Dropping it releases the tier.
#[derive(Debug)]
pub struct AdmissionPermit {
    tier: Arc<TierAdmission>,
    key: String,
    completed: AtomicBool,
}

impl AdmissionPermit {
    fn new(tier: Arc<TierAdmission>, key: String) -> Self {
        Self {
            tier,
            key,
            completed: AtomicBool::new(false),
        }
    }

    /// Mark this attempt successful exactly once. Streaming permits call this
    /// when the upstream stream completes cleanly; non-streaming permits call
    /// it after a usable 2xx body has been returned.
    pub fn record_success(&self) {
        if self
            .completed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.tier.record_success(&self.key);
        }
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        let inflight = self.tier.inflight.fetch_sub(1, Ordering::AcqRel) - 1;
        metrics::record_tier_inflight(&self.key, inflight);
    }
}

impl AdmissionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self, key: &str, provider_limit: Option<usize>) -> Arc<TierAdmission> {
        let mut tiers = self.tiers.lock().expect("admission state lock");
        tiers
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(TierAdmission::new(provider_limit)))
            .clone()
    }

    /// Install one route's configured limit as its AIMD ceiling. Later calls
    /// never reset AIMD state: raising the configured value widens future
    /// growth only, and lowering it clamps the current limit.
    pub fn configure(&self, key: &str, provider_limit: Option<usize>) {
        let state = self.state(key, provider_limit);
        state.set_ceiling(provider_limit);
        metrics::record_tier_limit(key, state.limit());
    }

    /// Acquire a slot if the route is below its current AIMD limit.
    pub fn acquire(
        self: &Arc<Self>,
        key: &str,
        provider_limit: Option<usize>,
    ) -> Option<AdmissionPermit> {
        self.configure(key, provider_limit);
        let state = self.state(key, provider_limit);
        let current = state.inflight.fetch_add(1, Ordering::AcqRel);
        let limit = state.limit.load(Ordering::Acquire);
        if current >= limit {
            state.inflight.fetch_sub(1, Ordering::AcqRel);
            metrics::record_admission_defer(key);
            return None;
        }
        metrics::record_tier_inflight(key, current + 1);
        metrics::record_tier_limit(key, limit);
        Some(AdmissionPermit::new(state, key.to_string()))
    }

    /// Is this route currently at or above its admission limit?
    pub fn at_capacity(&self, key: &str, provider_limit: Option<usize>) -> bool {
        self.configure(key, provider_limit);
        let state = self.state(key, provider_limit);
        state.inflight.load(Ordering::Acquire) >= state.limit.load(Ordering::Acquire)
    }

    /// A 429 cuts the local limit in half.
    pub fn record_rejection(&self, key: &str) {
        self.state(key, None).record_rejection(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aimd_rejection_halves_and_success_grows_bounded() {
        let tracker = AdmissionTracker::new();
        tracker.configure("p,m", Some(8));
        tracker.record_rejection("p,m");
        assert_eq!(tracker.state("p,m", None).limit(), 4);
        tracker.record_rejection("p,m");
        assert_eq!(tracker.state("p,m", None).limit(), 2);
        tracker.state("p,m", None).record_success("p,m");
        assert_eq!(tracker.state("p,m", None).limit(), 3);
    }

    #[test]
    fn configure_does_not_reset_an_aimd_cut() {
        let tracker = AdmissionTracker::new();
        tracker.configure("p,m", Some(8));
        tracker.record_rejection("p,m");
        tracker.configure("p,m", Some(8));
        tracker.at_capacity("p,m", Some(8));
        assert_eq!(tracker.state("p,m", None).limit(), 4);
    }

    #[test]
    fn acquire_defers_at_limit_and_releases() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(1));
        let permit = tracker.acquire("p,m", Some(1)).unwrap();
        assert!(tracker.at_capacity("p,m", Some(1)));
        assert!(tracker.acquire("p,m", Some(1)).is_none());
        drop(permit);
        assert!(!tracker.at_capacity("p,m", Some(1)));
    }

    /// A configured `maxInflight` is a ceiling, not only a starting point:
    /// AIMD growth after successes must never exceed it.
    #[test]
    fn permit_success_is_idempotent_and_respects_the_configured_ceiling() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(2));
        let permit = tracker.acquire("p,m", Some(2)).unwrap();
        permit.record_success();
        permit.record_success();
        assert_eq!(
            tracker.state("p,m", None).limit(),
            2,
            "success growth must not exceed the configured maxInflight"
        );

        // Without a provider ceiling, the global AIMD ceiling remains 64 even
        // when the default starting limit is 8.
        let free = Arc::new(AdmissionTracker::new());
        free.configure("free,m", None);
        assert_eq!(free.state("free,m", None).limit(), 8);
    }

    /// An unconfigured route's `maxInflight` is only its starting limit; AIMD
    /// still has access to the global ceiling after sustained successes.
    #[test]
    fn unconfigured_successes_grow_to_the_global_ceiling() {
        let tracker = AdmissionTracker::new();
        tracker.configure("free,m", None);
        let tier = tracker.state("free,m", None);

        for _ in 0..(DEFAULT_MAX_LIMIT - DEFAULT_INITIAL_LIMIT) {
            tier.record_success("free,m");
        }
        assert_eq!(tier.limit(), DEFAULT_MAX_LIMIT as u64);

        tier.record_success("free,m");
        assert_eq!(tier.limit(), DEFAULT_MAX_LIMIT as u64);
    }

    /// Raising a configured ceiling unblocks future AIMD growth but does not
    /// discard a cut that has already been applied.
    #[test]
    fn reconfiguration_raises_the_ceiling_without_resetting_a_cut() {
        let tracker = AdmissionTracker::new();
        tracker.configure("p,m", Some(8));
        tracker.record_rejection("p,m");
        tracker.configure("p,m", Some(32));

        let tier = tracker.state("p,m", None);
        assert_eq!(tier.limit(), 4);
        for _ in 0..28 {
            tier.record_success("p,m");
        }
        assert_eq!(tier.limit(), 32);
    }

    /// Each success contributes one atomic additive-increase step; no thread
    /// can overwrite another's increment with a stale limit.
    #[test]
    fn concurrent_successes_do_not_lose_additive_increments() {
        let tracker = AdmissionTracker::new();
        tracker.configure("p,m", None);
        let tier = tracker.state("p,m", None);

        std::thread::scope(|scope| {
            for _ in 0..DEFAULT_MAX_LIMIT {
                let tier = &tier;
                scope.spawn(move || tier.record_success("p,m"));
            }
        });

        assert_eq!(tier.limit(), DEFAULT_MAX_LIMIT as u64);
    }
}
