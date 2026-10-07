// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for phantom rate-limiting when upstream returns 200 with rate-limit headers.
//!
//! Z.AI (and other Anthropic-compatible providers) include `x-ratelimit-remaining`
//! and `x-ratelimit-reset` headers on every successful response as informational
//! warnings.  When `honor_ratelimit_headers` is false for a provider, CCR-Rust
//! must not treat these as blocking signals.

use std::time::{Duration, Instant};

use ccr_rust::ratelimit::RateLimitTracker;

// ---------------------------------------------------------------------------
// honor_ratelimit_headers = false  (Z.AI-style providers)
// ---------------------------------------------------------------------------

/// With honor_remaining=false, 200 responses with rate-limit headers should
/// never trigger tier skipping, even when remaining reaches 0.
#[test]
fn test_200_with_ratelimit_headers_not_treated_as_429() {
    let tracker = RateLimitTracker::new();
    let tier = "zai-tier";

    for remaining in (0..=9).rev() {
        let reset_at = Some(Instant::now() + Duration::from_secs(60));
        tracker.record_success(tier, Some(remaining), reset_at);

        assert!(
            !tracker.has_backoff(tier),
            "Tier should not have exponential backoff after {} successful requests \
             (remaining={})",
            10 - remaining,
            remaining,
        );
    }

    assert!(
        !tracker.should_skip_tier(tier, false),
        "Tier must not be skipped (honor_remaining=false) after 10 successful \
         200 responses, even when x-ratelimit-remaining reached 0"
    );
}

/// Actual 429 should still trigger backoff regardless of honor_remaining.
#[test]
fn test_actual_429_triggers_backoff() {
    let tracker = RateLimitTracker::new();
    let tier = "zai-tier";

    tracker.record_429(tier, Some(Duration::from_secs(5)));

    assert!(
        tracker.should_skip_tier(tier, false),
        "Tier must be skipped after a real 429, even with honor_remaining=false"
    );
    assert!(
        tracker.has_backoff(tier),
        "Tier must have exponential backoff after a real 429"
    );
}

/// record_success after a 429 should clear the backoff.
#[test]
fn test_success_clears_429_backoff() {
    let tracker = RateLimitTracker::new();
    let tier = "zai-tier";

    tracker.record_429(tier, Some(Duration::from_secs(1)));
    assert!(tracker.should_skip_tier(tier, false));

    tracker.record_success(tier, Some(5), None);

    assert!(
        !tracker.should_skip_tier(tier, false),
        "Tier must not be skipped after a successful request clears the 429 backoff"
    );
}

/// Full lifecycle: 10 successes → 429 → backoff → success → clear.
#[test]
fn test_full_lifecycle_200_then_429_then_recovery() {
    let tracker = RateLimitTracker::new();
    let tier = "zai-tier";

    // Phase 1: 10 requests return 200 with rate-limit headers → all succeed
    for i in (0..=9).rev() {
        let reset_at = Some(Instant::now() + Duration::from_secs(60));
        tracker.record_success(tier, Some(i), reset_at);
        assert!(
            !tracker.should_skip_tier(tier, false),
            "should_skip_tier(honor=false) must be false after success #{} (remaining={})",
            10 - i,
            i,
        );
    }

    // Phase 2: 1 request returns actual 429 → backoff triggers
    tracker.record_429(tier, Some(Duration::from_secs(2)));
    assert!(
        tracker.should_skip_tier(tier, false),
        "Tier must be skipped after a real 429"
    );

    // Phase 3: Next request succeeds → backoff clears
    tracker.record_success(tier, Some(10), None);
    assert!(
        !tracker.should_skip_tier(tier, false),
        "Tier must not be skipped after recovery"
    );
    assert!(
        !tracker.has_backoff(tier),
        "Backoff must be cleared after recovery"
    );
}

/// Remaining=0 with honor_remaining=false is informational — does NOT block.
#[test]
fn test_remaining_zero_informational_when_not_honored() {
    let tracker = RateLimitTracker::new();
    let tier = "zai-tier";

    let reset_at = Some(Instant::now() + Duration::from_secs(60));
    tracker.record_success(tier, Some(0), reset_at);

    assert!(
        !tracker.should_skip_tier(tier, false),
        "remaining=0 must not block when honor_remaining=false"
    );
}

// ---------------------------------------------------------------------------
// honor_ratelimit_headers = true  (default — providers with accurate headers)
// ---------------------------------------------------------------------------

/// With honor_remaining=true, remaining=0 SHOULD block the tier.
#[test]
fn test_remaining_zero_blocks_when_honored() {
    let tracker = RateLimitTracker::new();
    let tier = "anthropic-tier";

    let reset_at = Some(Instant::now() + Duration::from_secs(60));
    tracker.record_success(tier, Some(0), reset_at);

    assert!(
        tracker.should_skip_tier(tier, true),
        "remaining=0 must block when honor_remaining=true (provider headers are trusted)"
    );
}

/// With honor_remaining=true, remaining > 0 should NOT block.
#[test]
fn test_remaining_nonzero_does_not_block_when_honored() {
    let tracker = RateLimitTracker::new();
    let tier = "anthropic-tier";

    let reset_at = Some(Instant::now() + Duration::from_secs(60));
    tracker.record_success(tier, Some(5), reset_at);

    assert!(
        !tracker.should_skip_tier(tier, true),
        "remaining=5 must not block even with honor_remaining=true"
    );
}

// ---------------------------------------------------------------------------
// Overlapping-request ordering: a success must not clear a newer 429 backoff
// ---------------------------------------------------------------------------

/// A request that snapshotted the generation before another request's 429
/// must not clear that newer backoff when it finishes cleanly.
#[test]
fn test_stale_generation_success_preserves_newer_backoff() {
    let tracker = RateLimitTracker::new();
    let tier = "route";

    // Request A begins: snapshot the (empty) generation.
    let gen_a = tracker.generation(tier);

    // Request B gets rate-limited while A is in flight.
    tracker.record_429(tier, Some(Duration::from_secs(30)));
    assert!(tracker.has_backoff(tier));

    // Request A finishes cleanly afterwards.
    tracker.record_success_if_current(tier, gen_a, Some(5), None);

    assert!(
        tracker.has_backoff(tier),
        "success from before the 429 must not clear the newer backoff"
    );
    assert!(
        tracker.should_skip_tier(tier, true),
        "route must still be skipped for the remainder of B's backoff window"
    );
}

/// A success from the current generation still clears backoff normally.
#[test]
fn test_current_generation_success_clears_backoff() {
    let tracker = RateLimitTracker::new();
    let tier = "route";

    tracker.record_429(tier, Some(Duration::from_secs(1)));
    assert!(tracker.has_backoff(tier));

    let gen_b = tracker.generation(tier);
    tracker.record_success_if_current(tier, gen_b, Some(5), None);

    assert!(
        !tracker.has_backoff(tier),
        "success from the current generation must clear the backoff"
    );
    assert!(
        !tracker.should_skip_tier(tier, true),
        "route must be dispatchable again after a current-generation success"
    );
}

/// Header-derived quota info updates even when the backoff is preserved.
#[test]
fn test_stale_generation_success_still_updates_quota_headers() {
    let tracker = RateLimitTracker::new();
    let tier = "route";

    let gen_a = tracker.generation(tier);
    tracker.record_429(tier, Some(Duration::from_secs(30)));

    let reset_at = Some(Instant::now() + Duration::from_secs(60));
    tracker.record_success_if_current(tier, gen_a, Some(7), reset_at);

    assert!(tracker.has_backoff(tier));
    // The skip that follows comes from the preserved backoff window, not
    // from quota exhaustion: remaining=7 alone would never block.
    assert!(tracker.should_skip_tier(tier, true));
}

/// A 429 without a server Retry-After must only pace the tier briefly.
/// Coding-plan endpoints reject some requests under concurrency while still
/// admitting others, so escalating local backoff blanket-skips a tier that
/// remains partially available.
#[test]
fn test_429_without_retry_after_only_paces_briefly() {
    let tracker = RateLimitTracker::new();
    let tier = "zai-tier";

    for _ in 0..10 {
        tracker.record_429(tier, None);
    }

    let remaining = tracker
        .backoff_remaining(tier)
        .expect("a short pacing backoff should be active");
    assert!(
        remaining <= Duration::from_secs(1),
        "backoff must not escalate without server Retry-After, got {remaining:?}"
    );
}

/// A server-directed Retry-After is honored verbatim but capped at 60s.
#[test]
fn test_retry_after_honored_verbatim_and_capped() {
    let tracker = RateLimitTracker::new();

    tracker.record_429("capped-tier", Some(Duration::from_secs(120)));
    let remaining = tracker
        .backoff_remaining("capped-tier")
        .expect("backoff should be active");
    assert!(
        remaining <= Duration::from_secs(60),
        "backoff must be capped at 60s, got {remaining:?}"
    );

    tracker.record_429("exact-tier", Some(Duration::from_secs(30)));
    let remaining = tracker
        .backoff_remaining("exact-tier")
        .expect("backoff should be active");
    assert!(
        remaining > Duration::from_secs(25),
        "server-directed Retry-After must be honored, got {remaining:?}"
    );
}
