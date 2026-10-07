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
}

impl TierAdmission {
    fn new(initial_limit: usize) -> Self {
        let initial = initial_limit.clamp(DEFAULT_MIN_LIMIT, DEFAULT_MAX_LIMIT);
        Self {
            inflight: AtomicU64::new(0),
            limit: AtomicU64::new(initial as u64),
        }
    }

    fn record_success(&self, key: &str) {
        let old = self.limit.fetch_add(1, Ordering::AcqRel);
        let next = (old + 1).min(DEFAULT_MAX_LIMIT as u64);
        self.limit.store(next, Ordering::Release);
        metrics::record_tier_limit(key, next);
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
            .or_insert_with(|| {
                Arc::new(TierAdmission::new(
                    provider_limit.unwrap_or(DEFAULT_INITIAL_LIMIT),
                ))
            })
            .clone()
    }

    /// Install or update a provider's configured limit for one route.
    pub fn configure(&self, key: &str, provider_limit: Option<usize>) {
        if let Some(maximum) = provider_limit {
            let bounded = maximum.clamp(DEFAULT_MIN_LIMIT, DEFAULT_MAX_LIMIT) as u64;
            let state = self.state(key, provider_limit);
            state.limit.store(bounded, Ordering::Release);
            metrics::record_tier_limit(key, bounded);
        } else {
            // Touch the default state so gauges exist before the first attempt.
            let state = self.state(key, None);
            metrics::record_tier_limit(key, state.limit.load(Ordering::Acquire));
        }
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
        let state = self.state(key, None);
        let old = state.limit.fetch_max(1, Ordering::AcqRel);
        let next = (old / 2).max(DEFAULT_MIN_LIMIT as u64);
        state.limit.store(next, Ordering::Release);
        metrics::record_tier_limit(key, next);
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
        assert_eq!(tracker.state("p,m", None).limit.load(Ordering::Acquire), 4);
        tracker.record_rejection("p,m");
        assert_eq!(tracker.state("p,m", None).limit.load(Ordering::Acquire), 2);
        tracker.state("p,m", None).record_success("p,m");
        assert_eq!(tracker.state("p,m", None).limit.load(Ordering::Acquire), 3);
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

    #[test]
    fn permit_success_is_idempotent() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(2));
        let permit = tracker.acquire("p,m", Some(2)).unwrap();
        permit.record_success();
        permit.record_success();
        assert_eq!(tracker.state("p,m", None).limit.load(Ordering::Acquire), 3);
    }
}
