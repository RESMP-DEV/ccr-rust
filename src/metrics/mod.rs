// SPDX-License-Identifier: AGPL-3.0-or-later
mod handlers;
pub use handlers::*;

mod persistence;
use persistence::*;
pub use persistence::{clear_redis_persistence, init_persistence};

use lazy_static::lazy_static;
use parking_lot::RwLock;
use prometheus::{
    register_counter, register_counter_vec, register_gauge, register_gauge_vec,
    register_histogram_vec, Counter, CounterVec, Gauge, GaugeVec, HistogramVec,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use tiktoken_rs::cl100k_base;
use tracing::info;

use crate::frontend::FrontendType;
use crate::routing::EwmaTracker;

lazy_static! {
    static ref REQUESTS_TOTAL: CounterVec = register_counter_vec!(
        "ccr_requests_total",
        "Total number of requests per tier",
        &["tier"]
    )
    .unwrap();

    static ref REQUEST_DURATION: HistogramVec = register_histogram_vec!(
        "ccr_request_duration_seconds",
        "Request duration in seconds per tier",
        &["tier"],
        vec![0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
    )
    .unwrap();

    static ref FRONTEND_REQUESTS_TOTAL: CounterVec = register_counter_vec!(
        "ccr_frontend_requests_total",
        "Total number of requests per frontend",
        &["frontend"]
    )
    .unwrap();

    static ref FRONTEND_REQUEST_LATENCY: HistogramVec = register_histogram_vec!(
        "ccr_frontend_request_duration_seconds",
        "Request duration in seconds per frontend",
        &["frontend"],
        vec![0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
    )
    .unwrap();

    static ref FAILURES_TOTAL: CounterVec = register_counter_vec!(
        "ccr_failures_total",
        "Total number of failures per tier and reason",
        &["tier", "reason"]
    )
    .unwrap();

    static ref COST_USD_TOTAL: CounterVec = register_counter_vec!(
        "ccr_cost_usd_total",
        "Total estimated USD cost per tier (priced tiers only)",
        &["tier"]
    )
    .unwrap();

    static ref ACTIVE_STREAMS: Gauge = register_gauge!(
        "ccr_active_streams",
        "Current number of active SSE streams"
    )
    .unwrap();

    static ref ACTIVE_REQUESTS: Gauge = register_gauge!(
        "ccr_active_requests",
        "Current number of in-flight requests (streaming or non-streaming)"
    )
    .unwrap();

    // Usage reporting: token counters per tier
    static ref INPUT_TOKENS_TOTAL: CounterVec = register_counter_vec!(
        "ccr_input_tokens_total",
        "Total input tokens consumed per tier",
        &["tier"]
    )
    .unwrap();

    static ref OUTPUT_TOKENS_TOTAL: CounterVec = register_counter_vec!(
        "ccr_output_tokens_total",
        "Total output tokens generated per tier",
        &["tier"]
    )
    .unwrap();

    static ref CACHE_READ_TOKENS_TOTAL: CounterVec = register_counter_vec!(
        "ccr_cache_read_tokens_total",
        "Total cache read tokens per tier",
        &["tier"]
    )
    .unwrap();

    static ref CACHE_CREATION_TOKENS_TOTAL: CounterVec = register_counter_vec!(
        "ccr_cache_creation_tokens_total",
        "Total cache creation tokens per tier",
        &["tier"]
    )
    .unwrap();

    static ref TIER_EWMA_LATENCY: GaugeVec = register_gauge_vec!(
        "ccr_tier_ewma_latency_seconds",
        "EWMA latency per tier in seconds (alpha=0.3)",
        &["tier"]
    )
    .unwrap();

    static ref STREAM_BACKPRESSURE: Counter = register_counter!(
        "ccr_stream_backpressure_total",
        "Number of times an SSE stream producer blocked due to full channel buffer"
    )
    .unwrap();

    static ref PEAK_ACTIVE_STREAMS: Gauge = register_gauge!(
        "ccr_peak_active_streams",
        "High-water mark for concurrent SSE streams"
    )
    .unwrap();

    static ref REJECTED_STREAMS: Counter = register_counter!(
        "ccr_rejected_streams_total",
        "Number of streams rejected due to concurrency limit"
    )
    .unwrap();

    // Pre-request token audit: estimated input tokens before sending to backend
    static ref PRE_REQUEST_TOKENS: CounterVec = register_counter_vec!(
        "ccr_pre_request_tokens_total",
        "Estimated input tokens per tier and component before sending to backend",
        &["tier", "component"]
    )
    .unwrap();

    static ref PRE_REQUEST_TOKENS_HIST: HistogramVec = register_histogram_vec!(
        "ccr_pre_request_tokens",
        "Distribution of estimated pre-request token counts per tier",
        &["tier"],
        vec![100.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 25000.0, 50000.0, 100000.0, 200000.0]
    )
    .unwrap();

    static ref RATE_LIMIT_HITS: CounterVec = register_counter_vec!(
        "ccr_rate_limit_hits_total",
        "Number of 429 responses received per tier",
        &["tier"]
    )
    .unwrap();

    // Token drift verification: absolute difference between local estimate and upstream reported
    static ref TOKEN_DRIFT_ABS: GaugeVec = register_gauge_vec!(
        "ccr_token_drift_absolute",
        "Absolute difference (local_estimate - upstream_reported) of input tokens per tier",
        &["tier"]
    )
    .unwrap();

    static ref TOKEN_DRIFT_PCT: GaugeVec = register_gauge_vec!(
        "ccr_token_drift_pct",
        "Percentage drift ((local - upstream) / upstream * 100) of input tokens per tier",
        &["tier"]
    )
    .unwrap();

    static ref TOKEN_DRIFT_ALERTS: CounterVec = register_counter_vec!(
        "ccr_token_drift_alerts_total",
        "Number of times token drift exceeded the alert threshold per tier",
        &["tier", "severity"]
    )
    .unwrap();

    // TTFT: Time To First Token (from stream start to first content chunk)
    static ref TTFT_SECONDS: HistogramVec = register_histogram_vec!(
        "ccr_ttft_seconds",
        "Time to first token in seconds per tier",
        &["tier"],
        vec![0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 30.0]
    )
    .unwrap();

    // Output throughput: tokens per second
    static ref OUTPUT_TOKENS_PER_SECOND: HistogramVec = register_histogram_vec!(
        "ccr_output_tokens_per_second",
        "Output token throughput (tokens/sec) per tier",
        &["tier"],
        vec![1.0, 5.0, 10.0, 20.0, 40.0, 60.0, 80.0, 100.0, 150.0, 200.0]
    )
    .unwrap();

    static ref BPE: tiktoken_rs::CoreBPE = cl100k_base().expect("failed to load cl100k_base tokenizer");
}

const METRIC_REQUESTS_TOTAL: &str = "ccr_requests_total";
const METRIC_REQUEST_DURATION_SECONDS: &str = "ccr_request_duration_seconds";
const METRIC_FRONTEND_REQUESTS_TOTAL: &str = "ccr_frontend_requests_total";
const METRIC_FRONTEND_REQUEST_DURATION_SECONDS: &str = "ccr_frontend_request_duration_seconds";
const METRIC_FAILURES_TOTAL: &str = "ccr_failures_total";
const METRIC_PEAK_ACTIVE_STREAMS: &str = "ccr_peak_active_streams";
const METRIC_STREAM_BACKPRESSURE_TOTAL: &str = "ccr_stream_backpressure_total";
const METRIC_REJECTED_STREAMS_TOTAL: &str = "ccr_rejected_streams_total";
const METRIC_INPUT_TOKENS_TOTAL: &str = "ccr_input_tokens_total";
const METRIC_OUTPUT_TOKENS_TOTAL: &str = "ccr_output_tokens_total";
const METRIC_CACHE_READ_TOKENS_TOTAL: &str = "ccr_cache_read_tokens_total";
const METRIC_CACHE_CREATION_TOKENS_TOTAL: &str = "ccr_cache_creation_tokens_total";
const METRIC_COST_USD_TOTAL: &str = "ccr_cost_usd_total";
const METRIC_PRE_REQUEST_TOKENS_TOTAL: &str = "ccr_pre_request_tokens_total";
const METRIC_PRE_REQUEST_TOKENS: &str = "ccr_pre_request_tokens";
const METRIC_RATE_LIMIT_HITS_TOTAL: &str = "ccr_rate_limit_hits_total";
const METRIC_RATE_LIMIT_BACKOFFS_TOTAL: &str = "ccr_rate_limit_backoffs_total";
const METRIC_TIER_EWMA_LATENCY_SECONDS: &str = "ccr_tier_ewma_latency_seconds";
const METRIC_TOKEN_DRIFT_ABSOLUTE: &str = "ccr_token_drift_absolute";
const METRIC_TOKEN_DRIFT_PCT: &str = "ccr_token_drift_pct";
const METRIC_TOKEN_DRIFT_ALERTS_TOTAL: &str = "ccr_token_drift_alerts_total";
const METRIC_TTFT_SECONDS: &str = "ccr_ttft_seconds";
const METRIC_OUTPUT_TOKENS_PER_SECOND: &str = "ccr_output_tokens_per_second";

const REQUEST_DURATION_BUCKETS: &[f64] = &[0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0];
const PRE_REQUEST_TOKENS_BUCKETS: &[f64] = &[
    100.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 25000.0, 50000.0, 100000.0, 200000.0,
];

// Per-tier token drift state: tracks running totals for local estimates vs upstream reported.
// Fields: (local_estimate_sum, upstream_reported_sum, sample_count, last_drift_pct)
static TOKEN_DRIFT_STATE: RwLock<Option<HashMap<String, TokenDriftEntry>>> = RwLock::new(None);

/// Maximum number of pre-request audit entries retained in the ring buffer.
const AUDIT_LOG_CAPACITY: usize = 1024;

/// Ring buffer holding the most recent pre-request token audit entries.
static AUDIT_LOG: RwLock<Option<VecDeque<PreRequestAuditEntry>>> = RwLock::new(None);

/// A single pre-request token audit record capturing the estimated token
/// breakdown for a request before it is dispatched to a backend tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreRequestAuditEntry {
    /// ISO-8601 timestamp of when the audit was recorded.
    pub timestamp: String,
    /// Backend tier the request targets.
    pub tier: String,
    /// Estimated tokens from message content.
    pub message_tokens: u64,
    /// Estimated tokens from the system prompt.
    pub system_tokens: u64,
    /// Estimated tokens from tool definitions.
    pub tool_tokens: u64,
    /// Sum of all component token estimates.
    pub total_tokens: u64,
}

/// Percentage thresholds for drift severity classification.
const DRIFT_WARN_PCT: f64 = 10.0;
const DRIFT_ALERT_PCT: f64 = 25.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenDriftEntry {
    local_sum: u64,
    upstream_sum: u64,
    samples: u64,
    last_drift_pct: f64,
    last_local: u64,
    last_upstream: u64,
}

// Atomic counters for fast aggregate access without Prometheus iteration
pub static TOTAL_INPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
pub static TOTAL_OUTPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
pub static TOTAL_REQUESTS: AtomicU64 = AtomicU64::new(0);
pub static TOTAL_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Get the current number of active streams.
pub fn get_active_streams() -> f64 {
    ACTIVE_STREAMS.get()
}

/// Get the current number of active requests (streaming or non-streaming).
pub fn get_active_requests() -> f64 {
    ACTIVE_REQUESTS.get()
}

/// Increment or decrement the active requests counter.
/// Call with +1 when a request starts, -1 when it completes.
pub fn increment_active_requests(delta: i64) {
    if delta > 0 {
        ACTIVE_REQUESTS.add(delta as f64);
    } else {
        ACTIVE_REQUESTS.sub((-delta) as f64);
    }
}

fn frontend_label(frontend: FrontendType) -> &'static str {
    match frontend {
        FrontendType::Codex => "codex",
        FrontendType::ClaudeCode => "claude_code",
    }
}

pub fn record_request(tier: &str) {
    REQUESTS_TOTAL.with_label_values(&[tier]).inc();
    TOTAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
    persist_counter_inc(METRIC_REQUESTS_TOTAL, &[("tier", tier)], 1.0);
}

pub fn record_request_with_frontend(tier: &str, frontend: FrontendType) {
    REQUESTS_TOTAL.with_label_values(&[tier]).inc();
    TOTAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
    persist_counter_inc(METRIC_REQUESTS_TOTAL, &[("tier", tier)], 1.0);
    FRONTEND_REQUESTS_TOTAL
        .with_label_values(&[frontend_label(frontend)])
        .inc();
    persist_counter_inc(
        METRIC_FRONTEND_REQUESTS_TOTAL,
        &[("frontend", frontend_label(frontend))],
        1.0,
    );
}

/// Record request duration in the Prometheus histogram. EWMA tracking is handled
/// by `routing::EwmaTracker` directly; this only updates the histogram.
pub fn record_request_duration(tier: &str, duration: f64) {
    REQUEST_DURATION
        .with_label_values(&[tier])
        .observe(duration);
    persist_histogram_observe(METRIC_REQUEST_DURATION_SECONDS, &[("tier", tier)], duration);
}

/// Record request duration with frontend information.
pub fn record_request_duration_with_frontend(tier: &str, duration: f64, frontend: FrontendType) {
    REQUEST_DURATION
        .with_label_values(&[tier])
        .observe(duration);
    persist_histogram_observe(METRIC_REQUEST_DURATION_SECONDS, &[("tier", tier)], duration);
    FRONTEND_REQUEST_LATENCY
        .with_label_values(&[frontend_label(frontend)])
        .observe(duration);
    persist_histogram_observe(
        METRIC_FRONTEND_REQUEST_DURATION_SECONDS,
        &[("frontend", frontend_label(frontend))],
        duration,
    );
}

/// Sync the Prometheus EWMA gauge from the routing tracker. Called after the
/// tracker records a success or failure so the gauge stays in sync for scraping.
pub fn sync_ewma_gauge(tracker: &EwmaTracker) {
    for (tier, ewma, count) in tracker.get_all_latencies() {
        TIER_EWMA_LATENCY.with_label_values(&[&tier]).set(ewma);
        persist_gauge_set(METRIC_TIER_EWMA_LATENCY_SECONDS, &[("tier", &tier)], ewma);
        persist_ewma_state(&tier, ewma, count);
    }
}

pub fn record_failure(tier: &str, reason: &str) {
    FAILURES_TOTAL.with_label_values(&[tier, reason]).inc();
    TOTAL_FAILURES.fetch_add(1, Ordering::Relaxed);
    persist_counter_inc(
        METRIC_FAILURES_TOTAL,
        &[("tier", tier), ("reason", reason)],
        1.0,
    );
}

pub fn increment_active_streams(delta: i64) {
    if delta > 0 {
        ACTIVE_STREAMS.add(delta as f64);
        // Update high-water mark
        let current = ACTIVE_STREAMS.get();
        let peak = PEAK_ACTIVE_STREAMS.get();
        if current > peak {
            PEAK_ACTIVE_STREAMS.set(current);
            persist_gauge_max(METRIC_PEAK_ACTIVE_STREAMS, &[], current);
        }
    } else {
        ACTIVE_STREAMS.sub((-delta) as f64);
    }
}

/// Record that an SSE producer hit a full channel buffer (backpressure event).
pub fn record_stream_backpressure() {
    STREAM_BACKPRESSURE.inc();
    persist_counter_inc(METRIC_STREAM_BACKPRESSURE_TOTAL, &[], 1.0);
}

/// Record that a stream request was rejected due to concurrency limit.
#[allow(dead_code)]
pub fn record_rejected() {
    REJECTED_STREAMS.inc();
    persist_counter_inc(METRIC_REJECTED_STREAMS_TOTAL, &[], 1.0);
}

/// Record a 429 rate limit response from a backend tier.
pub fn record_rate_limit_hit(tier: &str) {
    RATE_LIMIT_HITS.with_label_values(&[tier]).inc();
    persist_counter_inc(METRIC_RATE_LIMIT_HITS_TOTAL, &[("tier", tier)], 1.0);
}

/// Persist 429 backoff counter state managed by `ratelimit.rs`.
pub fn record_rate_limit_backoff(tier: &str) {
    persist_counter_inc(METRIC_RATE_LIMIT_BACKOFFS_TOTAL, &[("tier", tier)], 1.0);
}

/// Per-tier throughput sample for the /v1/throughput endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputSample {
    pub ttft_seconds: f64,
    pub tokens_per_second: f64,
    pub output_tokens: u64,
    pub generation_seconds: f64,
    pub timestamp: String,
}

/// Per-tier throughput state: tracks recent samples for the API endpoint.
static THROUGHPUT_STATE: RwLock<Option<HashMap<String, VecDeque<ThroughputSample>>>> =
    RwLock::new(None);

/// Maximum number of throughput samples retained per tier.
const THROUGHPUT_SAMPLES_CAPACITY: usize = 256;

/// Record Time To First Token (TTFT) for a tier.
///
/// `ttft_secs` is the wall-clock duration from when the upstream HTTP response
/// started streaming to when the first content-bearing SSE chunk was received.
pub fn record_ttft(tier: &str, ttft_secs: f64) {
    TTFT_SECONDS.with_label_values(&[tier]).observe(ttft_secs);
    persist_histogram_observe(METRIC_TTFT_SECONDS, &[("tier", tier)], ttft_secs);
}

/// Record output token throughput (tok/s) for a tier.
///
/// `output_tokens` is the number of output tokens generated. `generation_secs`
/// is the wall-clock duration from first token to last token. `ttft_secs` is
/// the TTFT for this request (stored alongside for the /v1/throughput endpoint).
pub fn record_throughput(tier: &str, output_tokens: u64, generation_secs: f64, ttft_secs: f64) {
    if generation_secs <= 0.0 || output_tokens == 0 {
        return;
    }
    let tok_per_sec = output_tokens as f64 / generation_secs;
    OUTPUT_TOKENS_PER_SECOND
        .with_label_values(&[tier])
        .observe(tok_per_sec);
    persist_histogram_observe(
        METRIC_OUTPUT_TOKENS_PER_SECOND,
        &[("tier", tier)],
        tok_per_sec,
    );

    // Store sample for the /v1/throughput API
    let sample = ThroughputSample {
        ttft_seconds: ttft_secs,
        tokens_per_second: (tok_per_sec * 10.0).round() / 10.0,
        output_tokens,
        generation_seconds: (generation_secs * 1000.0).round() / 1000.0,
        timestamp: humantime::format_rfc3339_millis(SystemTime::now()).to_string(),
    };

    {
        let mut guard = THROUGHPUT_STATE.write();
        let state = guard.get_or_insert_with(HashMap::new);
        let samples = state
            .entry(tier.to_string())
            .or_insert_with(|| VecDeque::with_capacity(THROUGHPUT_SAMPLES_CAPACITY));
        if samples.len() >= THROUGHPUT_SAMPLES_CAPACITY {
            samples.pop_front();
        }
        samples.push_back(sample);
    }

    info!(
        tier = tier,
        output_tokens = output_tokens,
        ttft_ms = format!("{:.0}", ttft_secs * 1000.0),
        tok_per_sec = format!("{:.1}", tok_per_sec),
        generation_ms = format!("{:.0}", generation_secs * 1000.0),
        "throughput metrics"
    );
}

/// Read the current per-tier throughput state for the /v1/throughput handler.
pub fn get_throughput_state() -> HashMap<String, VecDeque<ThroughputSample>> {
    let guard = THROUGHPUT_STATE.read();
    guard.as_ref().cloned().unwrap_or_default()
}

/// Estimate token count for a JSON value by serializing it to a string and
/// running through the cl100k_base BPE tokenizer.
fn count_tokens_json(value: &serde_json::Value) -> u64 {
    let text = match value {
        serde_json::Value::String(s) => s.clone(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    };
    BPE.encode_ordinary(&text).len() as u64
}

/// Pre-request token audit: estimate the number of input tokens from the
/// request body before dispatching to the backend. Logs per-component counts
/// and records them to Prometheus for observability.
///
/// Components counted:
/// - `messages`: all message content
/// - `system`: system prompt (if present)
/// - `tools`: tool definitions (if present)
pub fn record_pre_request_tokens(
    tier: &str,
    messages: &[serde_json::Value],
    system: Option<&serde_json::Value>,
    tools: Option<&[serde_json::Value]>,
) -> u64 {
    let mut total: u64 = 0;

    // Messages
    let msg_tokens: u64 = messages.iter().map(count_tokens_json).sum();
    if msg_tokens > 0 {
        PRE_REQUEST_TOKENS
            .with_label_values(&[tier, "messages"])
            .inc_by(msg_tokens as f64);
        persist_counter_inc(
            METRIC_PRE_REQUEST_TOKENS_TOTAL,
            &[("tier", tier), ("component", "messages")],
            msg_tokens as f64,
        );
    }
    total += msg_tokens;

    // System prompt
    let sys_tokens = system.map(count_tokens_json).unwrap_or(0);
    if sys_tokens > 0 {
        PRE_REQUEST_TOKENS
            .with_label_values(&[tier, "system"])
            .inc_by(sys_tokens as f64);
        persist_counter_inc(
            METRIC_PRE_REQUEST_TOKENS_TOTAL,
            &[("tier", tier), ("component", "system")],
            sys_tokens as f64,
        );
    }
    total += sys_tokens;

    // Tool definitions
    let tool_tokens: u64 = tools
        .map(|t| t.iter().map(count_tokens_json).sum())
        .unwrap_or(0);
    if tool_tokens > 0 {
        PRE_REQUEST_TOKENS
            .with_label_values(&[tier, "tools"])
            .inc_by(tool_tokens as f64);
        persist_counter_inc(
            METRIC_PRE_REQUEST_TOKENS_TOTAL,
            &[("tier", tier), ("component", "tools")],
            tool_tokens as f64,
        );
    }
    total += tool_tokens;

    PRE_REQUEST_TOKENS_HIST
        .with_label_values(&[tier])
        .observe(total as f64);
    persist_histogram_observe(METRIC_PRE_REQUEST_TOKENS, &[("tier", tier)], total as f64);

    // Persist audit entry to the ring buffer for the /v1/token-audit endpoint.
    let entry = PreRequestAuditEntry {
        timestamp: humantime::format_rfc3339_millis(SystemTime::now()).to_string(),
        tier: tier.to_string(),
        message_tokens: msg_tokens,
        system_tokens: sys_tokens,
        tool_tokens,
        total_tokens: total,
    };

    {
        let mut guard = AUDIT_LOG.write();
        let log = guard.get_or_insert_with(|| VecDeque::with_capacity(AUDIT_LOG_CAPACITY));
        if log.len() >= AUDIT_LOG_CAPACITY {
            log.pop_front();
        }
        log.push_back(entry.clone());
    }

    persist_token_audit(&entry);

    info!(
        tier = tier,
        messages = msg_tokens,
        system = sys_tokens,
        tools = tool_tokens,
        total = total,
        "pre-request token audit"
    );

    total
}

/// Record token usage from a backend response.
pub fn record_usage(
    tier: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_creation: u64,
) {
    if input_tokens > 0 {
        INPUT_TOKENS_TOTAL
            .with_label_values(&[tier])
            .inc_by(input_tokens as f64);
        TOTAL_INPUT_TOKENS.fetch_add(input_tokens, Ordering::Relaxed);
        persist_counter_inc(
            METRIC_INPUT_TOKENS_TOTAL,
            &[("tier", tier)],
            input_tokens as f64,
        );
    }
    if output_tokens > 0 {
        OUTPUT_TOKENS_TOTAL
            .with_label_values(&[tier])
            .inc_by(output_tokens as f64);
        TOTAL_OUTPUT_TOKENS.fetch_add(output_tokens, Ordering::Relaxed);
        persist_counter_inc(
            METRIC_OUTPUT_TOKENS_TOTAL,
            &[("tier", tier)],
            output_tokens as f64,
        );
    }
    if cache_read > 0 {
        CACHE_READ_TOKENS_TOTAL
            .with_label_values(&[tier])
            .inc_by(cache_read as f64);
        persist_counter_inc(
            METRIC_CACHE_READ_TOKENS_TOTAL,
            &[("tier", tier)],
            cache_read as f64,
        );
    }
    if cache_creation > 0 {
        CACHE_CREATION_TOKENS_TOTAL
            .with_label_values(&[tier])
            .inc_by(cache_creation as f64);
        persist_counter_inc(
            METRIC_CACHE_CREATION_TOKENS_TOTAL,
            &[("tier", tier)],
            cache_creation as f64,
        );
    }
}

/// Record estimated USD cost for one completed request against its tier.
///
/// Callers resolve the model's pricing at request time (where the tier and
/// model are both known) and pass the estimated dollar cost here. Non-finite
/// or non-positive values are ignored so an unpriced tier contributes nothing.
pub fn record_cost(tier: &str, cost_usd: f64) {
    if cost_usd.is_finite() && cost_usd > 0.0 {
        COST_USD_TOTAL.with_label_values(&[tier]).inc_by(cost_usd);
        persist_counter_inc(METRIC_COST_USD_TOTAL, &[("tier", tier)], cost_usd);
    }
}

/// Compare local token estimate against upstream-reported input tokens.
///
/// Computes absolute and percentage drift, updates Prometheus gauges, and fires
/// alert counters when drift exceeds severity thresholds. The local estimate
/// comes from `record_pre_request_tokens` (tiktoken cl100k_base) and the
/// upstream value comes from the response `usage.input_tokens` field.
///
/// Drift = local_estimate - upstream_reported (positive means we over-estimated).
pub fn verify_token_usage(tier: &str, local_estimate: u64, upstream_input: u64) {
    if upstream_input == 0 {
        return;
    }

    let drift_abs = local_estimate as i64 - upstream_input as i64;
    let drift_pct = (drift_abs as f64 / upstream_input as f64) * 100.0;

    TOKEN_DRIFT_ABS
        .with_label_values(&[tier])
        .set(drift_abs as f64);
    TOKEN_DRIFT_PCT.with_label_values(&[tier]).set(drift_pct);
    persist_gauge_set(
        METRIC_TOKEN_DRIFT_ABSOLUTE,
        &[("tier", tier)],
        drift_abs as f64,
    );
    persist_gauge_set(METRIC_TOKEN_DRIFT_PCT, &[("tier", tier)], drift_pct);

    // Classify severity and fire alert counters
    let abs_pct = drift_pct.abs();
    if abs_pct >= DRIFT_ALERT_PCT {
        TOKEN_DRIFT_ALERTS
            .with_label_values(&[tier, "critical"])
            .inc();
        persist_counter_inc(
            METRIC_TOKEN_DRIFT_ALERTS_TOTAL,
            &[("tier", tier), ("severity", "critical")],
            1.0,
        );
        tracing::warn!(
            tier = tier,
            local = local_estimate,
            upstream = upstream_input,
            drift_pct = format!("{:.1}", drift_pct),
            "CRITICAL token drift: local estimate diverges >{}% from upstream",
            DRIFT_ALERT_PCT,
        );
    } else if abs_pct >= DRIFT_WARN_PCT {
        TOKEN_DRIFT_ALERTS
            .with_label_values(&[tier, "warning"])
            .inc();
        persist_counter_inc(
            METRIC_TOKEN_DRIFT_ALERTS_TOTAL,
            &[("tier", tier), ("severity", "warning")],
            1.0,
        );
        tracing::info!(
            tier = tier,
            local = local_estimate,
            upstream = upstream_input,
            drift_pct = format!("{:.1}", drift_pct),
            "token drift warning: local estimate diverges >{}% from upstream",
            DRIFT_WARN_PCT,
        );
    }

    // Update running state
    let mut guard = TOKEN_DRIFT_STATE.write();
    let state = guard.get_or_insert_with(HashMap::new);
    let entry = state.entry(tier.to_string()).or_insert(TokenDriftEntry {
        local_sum: 0,
        upstream_sum: 0,
        samples: 0,
        last_drift_pct: 0.0,
        last_local: 0,
        last_upstream: 0,
    });
    entry.local_sum += local_estimate;
    entry.upstream_sum += upstream_input;
    entry.samples += 1;
    entry.last_drift_pct = drift_pct;
    entry.last_local = local_estimate;
    entry.last_upstream = upstream_input;
    persist_token_drift_state(tier, entry);
}

#[cfg(test)]
mod tests {
    use super::{key_matches_persistence_prefix, record_pre_request_tokens, AUDIT_LOG};

    #[test]
    fn key_prefix_match_accepts_prefix_root_and_namespace() {
        assert!(key_matches_persistence_prefix(
            "ccr-rust:persistence:v1",
            "ccr-rust:persistence:v1"
        ));
        assert!(key_matches_persistence_prefix(
            "ccr-rust:persistence:v1:counter:ccr_requests_total",
            "ccr-rust:persistence:v1"
        ));
    }

    #[test]
    fn key_prefix_match_rejects_partial_or_unrelated_prefixes() {
        assert!(!key_matches_persistence_prefix(
            "ccr-rust:persistence:v10:counter:ccr_requests_total",
            "ccr-rust:persistence:v1"
        ));
        assert!(!key_matches_persistence_prefix(
            "another-prefix:counter:ccr_requests_total",
            "ccr-rust:persistence:v1"
        ));
    }

    #[test]
    fn token_audit_records_tool_tokens() {
        use serde_json::json;

        let body_str = serde_json::to_string(&json!({
            "model": "test-model",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "system": "You are a helpful assistant with calculator and search tools.",
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "calculator",
                        "description": "Perform arithmetic",
                        "parameters": {"type": "object", "properties": {}}
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "search",
                        "description": "Search the web",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }
            ]
        }))
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_str).unwrap();

        let messages = body["messages"].as_array().unwrap().clone();
        let system = Some(&body["system"]);
        let tools = Some(body["tools"].as_array().unwrap().as_slice());

        let total = record_pre_request_tokens("test_tier", &messages, system, tools);

        // Read the most recent audit entry from the ring buffer.
        let guard = AUDIT_LOG.read();
        let log = guard.as_ref().expect("audit log should exist");
        let entry = log
            .back()
            .expect("audit log should have at least one entry");

        assert_eq!(entry.tier, "test_tier");
        assert!(entry.message_tokens > 0, "message_tokens should be > 0");
        assert!(entry.system_tokens > 0, "system_tokens should be > 0");
        assert!(entry.tool_tokens > 0, "tool_tokens should be > 0");
        assert_eq!(
            entry.total_tokens,
            entry.message_tokens + entry.system_tokens + entry.tool_tokens,
            "total should equal sum of components"
        );
        assert_eq!(
            entry.total_tokens, total,
            "returned total should match audit entry total"
        );
    }

    #[test]
    fn token_audit_includes_tool_tokens() {
        // Focused coverage: verify PreRequestAuditEntry serializes with tool_tokens field.
        use super::PreRequestAuditEntry;

        let entry = PreRequestAuditEntry {
            timestamp: "2024-01-01T00:00:00.000Z".to_string(),
            tier: "test_tier".to_string(),
            message_tokens: 10,
            system_tokens: 20,
            tool_tokens: 30,
            total_tokens: 60,
        };

        let json = serde_json::to_string(&entry).expect("should serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("should parse");

        // Verify all fields are present including tool_tokens
        assert!(
            parsed.get("timestamp").is_some(),
            "timestamp should be in JSON"
        );
        assert!(parsed.get("tier").is_some(), "tier should be in JSON");
        assert!(
            parsed.get("message_tokens").is_some(),
            "message_tokens should be in JSON"
        );
        assert!(
            parsed.get("system_tokens").is_some(),
            "system_tokens should be in JSON"
        );
        assert!(
            parsed.get("tool_tokens").is_some(),
            "tool_tokens should be in JSON"
        );
        assert!(
            parsed.get("total_tokens").is_some(),
            "total_tokens should be in JSON"
        );

        // Verify the tool_tokens value
        assert_eq!(
            parsed["tool_tokens"].as_u64(),
            Some(30),
            "tool_tokens value should be 30"
        );
        assert_eq!(
            parsed["message_tokens"].as_u64(),
            Some(10),
            "message_tokens should be 10"
        );
        assert_eq!(
            parsed["system_tokens"].as_u64(),
            Some(20),
            "system_tokens should be 20"
        );
        assert_eq!(
            parsed["total_tokens"].as_u64(),
            Some(60),
            "total_tokens should be 60"
        );
    }
}
