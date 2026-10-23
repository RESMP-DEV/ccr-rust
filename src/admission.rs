// SPDX-License-Identifier: AGPL-3.0-or-later
//! Adaptive per-tier in-flight admission control.
//!
//! A no-guidance 429 often means the plan is admitting some requests while
//! rejecting others. A global backoff window over-corrects by idling a tier
//! that could still make progress. AIMD admission limits concurrency locally:
//! a rejection halves the tier limit, while a completed success grows it by
//! one bounded step.
//!
//! Request count is not the only thing a plan can run out of. A provider
//! whose 429s come from concurrent prefill volume (several large requests
//! prefilling at once) still shows a low request count when it collides, so
//! the count-only gate cannot see the collision coming. A provider can
//! therefore configure a token budget: admission then also requires the sum
//! of the in-flight request estimates to stay inside that budget. The token
//! budget is a configured ceiling, never an AIMD target, because a 429 that
//! stems from token volume must not shrink the request-count limit that the
//! AIMD controller owns.

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
    /// Estimated prompt tokens of the attempts currently in flight.
    inflight_tokens: AtomicU64,
    /// Configured token budget; zero means the route has no token budget.
    token_ceiling: AtomicU64,
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

    fn new(provider_limit: Option<usize>, token_limit: Option<u64>) -> Self {
        let (initial, ceiling) = Self::bounds(provider_limit);
        Self {
            inflight: AtomicU64::new(0),
            limit: AtomicU64::new(initial),
            ceiling: AtomicU64::new(ceiling),
            inflight_tokens: AtomicU64::new(0),
            token_ceiling: AtomicU64::new(token_limit.unwrap_or(0)),
        }
    }

    fn limit(&self) -> u64 {
        self.limit.load(Ordering::Acquire)
    }

    /// Re-apply a configured ceiling. Raising it never lifts an existing AIMD
    /// cut; lowering it clamps the live limit immediately so an operator
    /// reduction is honored without waiting for traffic.
    ///
    /// The token budget is applied the same way but never AIMD-adjusted: it
    /// is replaced by whatever the configuration says, and zero disables it.
    fn set_ceiling(&self, provider_limit: Option<usize>, token_limit: Option<u64>) {
        let (_, ceiling) = Self::bounds(provider_limit);
        self.ceiling.store(ceiling, Ordering::Release);
        let _ = self
            .limit
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let capped = current.min(ceiling);
                (capped != current).then_some(capped)
            });
        self.token_ceiling
            .store(token_limit.unwrap_or(0), Ordering::Release);
    }

    /// Reserve `estimate` tokens inside the configured token budget.
    ///
    /// The reservation is a compare-exchange loop, so two concurrent
    /// acquires cannot both see room for the same budget, and a request that
    /// does not fit never lands in the in-flight total.
    fn try_reserve_tokens(&self, estimate: u64, token_ceiling: u64) -> bool {
        let mut current = self.inflight_tokens.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(estimate);
            if next > token_ceiling {
                return false;
            }
            match self.inflight_tokens.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    /// Return `amount` tokens to the budget and report the remaining total.
    ///
    /// The subtraction saturates, so a permit whose reservation was already
    /// released cannot wrap the counter into a huge value that would then
    /// block the route forever.
    fn release_tokens(&self, amount: u64) -> u64 {
        let _ = self
            .inflight_tokens
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(amount))
            });
        self.inflight_tokens.load(Ordering::Acquire)
    }

    /// Additive increase. The clamp is part of the same atomic transition as
    /// the increment, so concurrent successes cannot lose a step and no
    /// observer can see a limit above the ceiling.
    fn record_success(&self, key: &str) {
        let grown = self
            .limit
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
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
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
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
    /// Tokens this attempt reserved from the tier's token budget. Zero when
    /// the route has no token budget, so the release is a no-op there.
    tokens: u64,
}

impl AdmissionPermit {
    fn new(tier: Arc<TierAdmission>, key: String, tokens: u64) -> Self {
        Self {
            tier,
            key,
            completed: AtomicBool::new(false),
            tokens,
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
        if self.tokens > 0 {
            let remaining = self.tier.release_tokens(self.tokens);
            metrics::record_tier_inflight_tokens(&self.key, remaining);
        }
    }
}

impl AdmissionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(
        &self,
        key: &str,
        provider_limit: Option<usize>,
        token_limit: Option<u64>,
    ) -> Arc<TierAdmission> {
        let mut tiers = self.tiers.lock().expect("admission state lock");
        tiers
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(TierAdmission::new(provider_limit, token_limit)))
            .clone()
    }

    /// Install one route's configured limit as its AIMD ceiling. Later calls
    /// never reset AIMD state: raising the configured value widens future
    /// growth only, and lowering it clamps the current limit.
    ///
    /// The token budget is installed the same way: it is the configured
    /// ceiling for in-flight tokens, and `None` disables the token gate.
    pub fn configure(&self, key: &str, provider_limit: Option<usize>, token_limit: Option<u64>) {
        let state = self.state(key, provider_limit, token_limit);
        state.set_ceiling(provider_limit, token_limit);
        metrics::record_tier_limit(key, state.limit());
    }

    /// Acquire a slot if the route is below its current AIMD limit and, when
    /// the route has a token budget, if the attempt's token estimate still
    /// fits inside that budget.
    ///
    /// The count gate and the token gate are independent: a request can be
    /// admitted by the count and deferred by the token budget, or the other
    /// way around. Both deferrals are recorded as admission defers, because
    /// from the caller's side the outcome is the same: no slot was taken.
    pub fn acquire(
        self: &Arc<Self>,
        key: &str,
        provider_limit: Option<usize>,
        token_estimate: u64,
        provider_token_limit: Option<u64>,
    ) -> Option<AdmissionPermit> {
        self.configure(key, provider_limit, provider_token_limit);
        let state = self.state(key, provider_limit, provider_token_limit);
        let current = state.inflight.fetch_add(1, Ordering::AcqRel);
        let limit = state.limit.load(Ordering::Acquire);
        if current >= limit {
            state.inflight.fetch_sub(1, Ordering::AcqRel);
            metrics::record_admission_defer(key);
            return None;
        }
        // Token budget gate. `configure` just installed
        // `provider_token_limit` as the tier's token ceiling, so the ceiling
        // used here is the one the route is configured with right now.
        let mut reserved = 0;
        if let Some(token_ceiling) = provider_token_limit {
            if !state.try_reserve_tokens(token_estimate, token_ceiling) {
                state.inflight.fetch_sub(1, Ordering::AcqRel);
                metrics::record_admission_defer(key);
                return None;
            }
            reserved = token_estimate;
            metrics::record_tier_inflight_tokens(
                key,
                state.inflight_tokens.load(Ordering::Acquire),
            );
        }
        metrics::record_tier_inflight(key, current + 1);
        metrics::record_tier_limit(key, limit);
        Some(AdmissionPermit::new(state, key.to_string(), reserved))
    }

    /// Is this route currently at or above its admission limit, or does the
    /// given request estimate no longer fit inside its token budget?
    ///
    /// The estimate matters only for the token gate: a route with no token
    /// budget is at capacity exactly when its request count reaches the
    /// AIMD limit.
    pub fn at_capacity(
        &self,
        key: &str,
        provider_limit: Option<usize>,
        token_estimate: u64,
        provider_token_limit: Option<u64>,
    ) -> bool {
        self.configure(key, provider_limit, provider_token_limit);
        let state = self.state(key, provider_limit, provider_token_limit);
        if state.inflight.load(Ordering::Acquire) >= state.limit.load(Ordering::Acquire) {
            return true;
        }
        if let Some(token_ceiling) = provider_token_limit {
            let tokens = state.inflight_tokens.load(Ordering::Acquire);
            if tokens.saturating_add(token_estimate) > token_ceiling {
                return true;
            }
        }
        false
    }

    /// A 429 cuts the local limit in half.
    pub fn record_rejection(&self, key: &str) {
        self.state(key, None, None).record_rejection(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Estimated tokens currently reserved by a route.
    fn tokens_in_flight(tracker: &AdmissionTracker, key: &str) -> u64 {
        tracker
            .state(key, None, None)
            .inflight_tokens
            .load(Ordering::Acquire)
    }

    /// Requests currently in flight on a route.
    fn count_in_flight(tracker: &AdmissionTracker, key: &str) -> u64 {
        tracker
            .state(key, None, None)
            .inflight
            .load(Ordering::Acquire)
    }

    #[test]
    fn aimd_rejection_halves_and_success_grows_bounded() {
        let tracker = AdmissionTracker::new();
        tracker.configure("p,m", Some(8), None);
        tracker.record_rejection("p,m");
        assert_eq!(tracker.state("p,m", None, None).limit(), 4);
        tracker.record_rejection("p,m");
        assert_eq!(tracker.state("p,m", None, None).limit(), 2);
        tracker.state("p,m", None, None).record_success("p,m");
        assert_eq!(tracker.state("p,m", None, None).limit(), 3);
    }

    #[test]
    fn configure_does_not_reset_an_aimd_cut() {
        let tracker = AdmissionTracker::new();
        tracker.configure("p,m", Some(8), None);
        tracker.record_rejection("p,m");
        tracker.configure("p,m", Some(8), None);
        tracker.at_capacity("p,m", Some(8), 0, None);
        assert_eq!(tracker.state("p,m", None, None).limit(), 4);
    }

    #[test]
    fn acquire_defers_at_limit_and_releases() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(1), None);
        let permit = tracker.acquire("p,m", Some(1), 0, None).unwrap();
        assert!(tracker.at_capacity("p,m", Some(1), 0, None));
        assert!(tracker.acquire("p,m", Some(1), 0, None).is_none());
        drop(permit);
        assert!(!tracker.at_capacity("p,m", Some(1), 0, None));
    }

    /// A configured `maxInflight` is a ceiling, not only a starting point:
    /// AIMD growth after successes must never exceed it.
    #[test]
    fn permit_success_is_idempotent_and_respects_the_configured_ceiling() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(2), None);
        let permit = tracker.acquire("p,m", Some(2), 0, None).unwrap();
        permit.record_success();
        permit.record_success();
        assert_eq!(
            tracker.state("p,m", None, None).limit(),
            2,
            "success growth must not exceed the configured maxInflight"
        );

        // Without a provider ceiling, the global AIMD ceiling remains 64 even
        // when the default starting limit is 8.
        let free = Arc::new(AdmissionTracker::new());
        free.configure("free,m", None, None);
        assert_eq!(free.state("free,m", None, None).limit(), 8);
    }

    /// An unconfigured route's `maxInflight` is only its starting limit; AIMD
    /// still has access to the global ceiling after sustained successes.
    #[test]
    fn unconfigured_successes_grow_to_the_global_ceiling() {
        let tracker = AdmissionTracker::new();
        tracker.configure("free,m", None, None);
        let tier = tracker.state("free,m", None, None);

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
        tracker.configure("p,m", Some(8), None);
        tracker.record_rejection("p,m");
        tracker.configure("p,m", Some(32), None);

        let tier = tracker.state("p,m", None, None);
        assert_eq!(tier.limit(), 4);
        for _ in 0..28 {
            tier.record_success("p,m");
        }
        assert_eq!(tier.limit(), 32);
    }

    /// A configured token budget defers an attempt whose estimate does not
    /// fit beside the tokens already in flight, and releasing the in-flight
    /// attempt restores admission.
    ///
    /// The deferral must also leave the request count untouched, so a smaller
    /// request can still be admitted beside the deferred one.
    #[test]
    fn token_budget_defers_at_ceiling_and_release_restores_admission() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(4), Some(1_000));

        let first = tracker
            .acquire("p,m", Some(4), 600, Some(1_000))
            .expect("600 tokens fit inside a 1000 token budget");
        assert!(tracker.acquire("p,m", Some(4), 600, Some(1_000)).is_none());
        assert!(
            tracker.at_capacity("p,m", Some(4), 600, Some(1_000)),
            "a request that does not fit inside the token budget is at capacity"
        );
        assert_eq!(
            count_in_flight(&tracker, "p,m"),
            1,
            "a token deferral must not leave the count slot taken"
        );
        assert!(tracker.acquire("p,m", Some(4), 100, Some(1_000)).is_some());

        drop(first);
        assert_eq!(
            tokens_in_flight(&tracker, "p,m"),
            0,
            "dropping the permit returns its tokens to the budget"
        );
        assert!(tracker.acquire("p,m", Some(4), 600, Some(1_000)).is_some());
    }

    /// A route without a token budget is gated by request count only: an
    /// arbitrarily large estimate must not defer anything.
    #[test]
    fn no_token_budget_leaves_admission_count_only() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(4), None);

        let permit = tracker
            .acquire("p,m", Some(4), 10_000_000, None)
            .expect("a route without a token budget never defers on tokens");
        assert!(!tracker.at_capacity("p,m", Some(4), 10_000_000, None));
        drop(permit);
    }

    /// The count gate keeps working next to the token gate: a route at its
    /// request limit defers even though the token budget has room.
    #[test]
    fn count_limit_defer_is_unaffected_by_the_token_budget() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(1), Some(100_000));

        let permit = tracker
            .acquire("p,m", Some(1), 10, Some(100_000))
            .expect("the first attempt fits both gates");
        assert!(tracker.acquire("p,m", Some(1), 10, Some(100_000)).is_none());
        drop(permit);
        assert!(tracker.acquire("p,m", Some(1), 10, Some(100_000)).is_some());
    }

    /// An attempt whose estimate fits inside the budget is admitted, and the
    /// budget only reports capacity once the estimate no longer fits.
    #[test]
    fn token_acquire_below_ceiling_succeeds() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(4), Some(4_000));

        let permit = tracker
            .acquire("p,m", Some(4), 1_000, Some(4_000))
            .expect("1000 tokens fit inside a 4000 token budget");
        assert_eq!(tokens_in_flight(&tracker, "p,m"), 1_000);
        assert!(
            !tracker.at_capacity("p,m", Some(4), 3_000, Some(4_000)),
            "an estimate that exactly fills the budget still fits"
        );
        assert!(tracker.at_capacity("p,m", Some(4), 3_001, Some(4_000)));
        drop(permit);
    }

    /// Dropping a permit returns exactly the tokens it reserved, so the
    /// budget does not leak across attempts.
    #[test]
    fn permit_drop_subtracts_the_reserved_tokens() {
        let tracker = Arc::new(AdmissionTracker::new());
        tracker.configure("p,m", Some(4), Some(1_000));

        let first = tracker
            .acquire("p,m", Some(4), 800, Some(1_000))
            .expect("800 tokens fit inside a 1000 token budget");
        let second = tracker
            .acquire("p,m", Some(4), 200, Some(1_000))
            .expect("200 more tokens fit beside the 800 already in flight");
        assert_eq!(tokens_in_flight(&tracker, "p,m"), 1_000);

        drop(first);
        assert_eq!(
            tokens_in_flight(&tracker, "p,m"),
            200,
            "the dropped permit returns only its own reservation"
        );
        drop(second);
        assert_eq!(
            tokens_in_flight(&tracker, "p,m"),
            0,
            "the budget is empty once every permit is dropped"
        );
    }

    /// Each success contributes one atomic additive-increase step; no thread
    /// can overwrite another's increment with a stale limit.
    #[test]
    fn concurrent_successes_do_not_lose_additive_increments() {
        let tracker = AdmissionTracker::new();
        tracker.configure("p,m", None, None);
        let tier = tracker.state("p,m", None, None);

        std::thread::scope(|scope| {
            for _ in 0..DEFAULT_MAX_LIMIT {
                let tier = &tier;
                scope.spawn(move || tier.record_success("p,m"));
            }
        });

        assert_eq!(tier.limit(), DEFAULT_MAX_LIMIT as u64);
    }
}
