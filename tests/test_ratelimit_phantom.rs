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
