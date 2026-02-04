use crate::ratelimit::RateLimitTracker;
use std::sync::Arc;
use std::time::Instant;

/// Context for token verification on streaming responses.
pub struct StreamVerifyCtx {
    pub tier_name: String,
    pub local_estimate: u64,
    pub ratelimit_tracker: Option<Arc<RateLimitTracker>>,
    pub rate_limit_info: Option<(Option<u32>, Option<Instant>)>,
}
