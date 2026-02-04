use axum::response::IntoResponse;
use axum::Json;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use prometheus::core::Collector;
use prometheus::{
    register_counter, register_counter_vec, register_gauge, register_gauge_vec,
    register_histogram_vec, Counter, CounterVec, Encoder, Gauge, GaugeVec, HistogramVec,
    TextEncoder,
};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use tiktoken_rs::cl100k_base;
use tracing::info;

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

    static ref FAILURES_TOTAL: CounterVec = register_counter_vec!(
        "ccr_failures_total",
        "Total number of failures per tier and reason",
        &["tier", "reason"]
    )
    .unwrap();

    static ref ACTIVE_STREAMS: Gauge = register_gauge!(
        "ccr_active_streams",
        "Current number of active SSE streams"
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

    static ref BPE: tiktoken_rs::CoreBPE = cl100k_base().expect("failed to load cl100k_base tokenizer");
}

// Per-tier token drift state: tracks running totals for local estimates vs upstream reported.
// Fields: (local_estimate_sum, upstream_reported_sum, sample_count, last_drift_pct)
static TOKEN_DRIFT_STATE: RwLock<Option<HashMap<String, TokenDriftEntry>>> = RwLock::new(None);

/// Maximum number of pre-request audit entries retained in the ring buffer.
const AUDIT_LOG_CAPACITY: usize = 1024;

/// Ring buffer holding the most recent pre-request token audit entries.
static AUDIT_LOG: RwLock<Option<VecDeque<PreRequestAuditEntry>>> = RwLock::new(None);

/// A single pre-request token audit record capturing the estimated token
/// breakdown for a request before it is dispatched to a backend tier.
#[derive(Debug, Clone, Serialize)]
pub struct PreRequestAuditEntry {
    /// ISO-8601 timestamp of when the audit was recorded.
    pub timestamp: String,
    /// Backend tier the request targets.
    pub tier: String,
    /// Estimated tokens from message content.
    pub message_tokens: u64,
    /// Estimated tokens from the system prompt.
    pub system_tokens: u64,
    /// Sum of all component token estimates.
    pub total_tokens: u64,
}

/// Percentage thresholds for drift severity classification.
const DRIFT_WARN_PCT: f64 = 10.0;
const DRIFT_ALERT_PCT: f64 = 25.0;

#[derive(Debug, Clone)]
struct TokenDriftEntry {
    local_sum: u64,
    upstream_sum: u64,
    samples: u64,
    last_drift_pct: f64,
    last_local: u64,
    last_upstream: u64,
}

// Atomic counters for fast aggregate access without Prometheus iteration
static TOTAL_INPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
static TOTAL_OUTPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
static TOTAL_REQUESTS: AtomicU64 = AtomicU64::new(0);
static TOTAL_FAILURES: AtomicU64 = AtomicU64::new(0);

pub fn record_request(tier: &str) {
    REQUESTS_TOTAL.with_label_values(&[tier]).inc();
    TOTAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
}

/// Record request duration in the Prometheus histogram. EWMA tracking is handled
/// by `routing::EwmaTracker` directly; this only updates the histogram.
pub fn record_request_duration(tier: &str, duration: f64) {
    REQUEST_DURATION.with_label_values(&[tier]).observe(duration);
}

/// Sync the Prometheus EWMA gauge from the routing tracker. Called after the
/// tracker records a success or failure so the gauge stays in sync for scraping.
pub fn sync_ewma_gauge(tracker: &EwmaTracker) {
    for (tier, ewma, _count) in tracker.get_all_latencies() {
        TIER_EWMA_LATENCY.with_label_values(&[&tier]).set(ewma);
    }
}

pub fn record_failure(tier: &str, reason: &str) {
    FAILURES_TOTAL.with_label_values(&[tier, reason]).inc();
    TOTAL_FAILURES.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_active_streams(delta: i64) {
    if delta > 0 {
        ACTIVE_STREAMS.add(delta as f64);
        // Update high-water mark
        let current = ACTIVE_STREAMS.get();
        let peak = PEAK_ACTIVE_STREAMS.get();
        if current > peak {
            PEAK_ACTIVE_STREAMS.set(current);
        }
    } else {
        ACTIVE_STREAMS.sub((-delta) as f64);
    }
}

/// Record that an SSE producer hit a full channel buffer (backpressure event).
pub fn record_stream_backpressure() {
    STREAM_BACKPRESSURE.inc();
}

/// Record that a stream request was rejected due to concurrency limit.
pub fn record_rejected() {
    REJECTED_STREAMS.inc();
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
    let msg_tokens: u64 = messages.iter().map(|m| count_tokens_json(m)).sum();
    if msg_tokens > 0 {
        PRE_REQUEST_TOKENS
            .with_label_values(&[tier, "messages"])
            .inc_by(msg_tokens as f64);
    }
    total += msg_tokens;

    // System prompt
    let sys_tokens = system.map(count_tokens_json).unwrap_or(0);
    if sys_tokens > 0 {
        PRE_REQUEST_TOKENS
            .with_label_values(&[tier, "system"])
            .inc_by(sys_tokens as f64);
    }
    total += sys_tokens;

    // Tool definitions
    let tool_tokens: u64 = tools
        .map(|t| t.iter().map(|v| count_tokens_json(v)).sum())
        .unwrap_or(0);
    if tool_tokens > 0 {
        PRE_REQUEST_TOKENS
            .with_label_values(&[tier, "tools"])
            .inc_by(tool_tokens as f64);
    }
    total += tool_tokens;

    PRE_REQUEST_TOKENS_HIST
        .with_label_values(&[tier])
        .observe(total as f64);

    // Persist audit entry to the ring buffer for the /v1/token-audit endpoint.
    let entry = PreRequestAuditEntry {
        timestamp: humantime::format_rfc3339_millis(SystemTime::now()).to_string(),
        tier: tier.to_string(),
        message_tokens: msg_tokens,
        system_tokens: sys_tokens,
        total_tokens: total,
    };

    {
        let mut guard = AUDIT_LOG.write();
        let log = guard.get_or_insert_with(|| VecDeque::with_capacity(AUDIT_LOG_CAPACITY));
        if log.len() >= AUDIT_LOG_CAPACITY {
            log.pop_front();
        }
        log.push_back(entry);
    }

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
pub fn record_usage(tier: &str, input_tokens: u64, output_tokens: u64, cache_read: u64, cache_creation: u64) {
    if input_tokens > 0 {
        INPUT_TOKENS_TOTAL.with_label_values(&[tier]).inc_by(input_tokens as f64);
        TOTAL_INPUT_TOKENS.fetch_add(input_tokens, Ordering::Relaxed);
    }
    if output_tokens > 0 {
        OUTPUT_TOKENS_TOTAL.with_label_values(&[tier]).inc_by(output_tokens as f64);
        TOTAL_OUTPUT_TOKENS.fetch_add(output_tokens, Ordering::Relaxed);
    }
    if cache_read > 0 {
        CACHE_READ_TOKENS_TOTAL.with_label_values(&[tier]).inc_by(cache_read as f64);
    }
    if cache_creation > 0 {
        CACHE_CREATION_TOKENS_TOTAL.with_label_values(&[tier]).inc_by(cache_creation as f64);
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
    TOKEN_DRIFT_PCT
        .with_label_values(&[tier])
        .set(drift_pct);

    // Classify severity and fire alert counters
    let abs_pct = drift_pct.abs();
    if abs_pct >= DRIFT_ALERT_PCT {
        TOKEN_DRIFT_ALERTS
            .with_label_values(&[tier, "critical"])
            .inc();
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
}

/// Per-tier drift summary for the /v1/token-drift JSON endpoint.
#[derive(Debug, Serialize)]
pub struct TierTokenDrift {
    pub tier: String,
    pub samples: u64,
    pub cumulative_local: u64,
    pub cumulative_upstream: u64,
    pub cumulative_drift_pct: f64,
    pub last_local: u64,
    pub last_upstream: u64,
    pub last_drift_pct: f64,
}

/// Handler for GET /v1/token-drift - returns per-tier token verification summary.
pub async fn token_drift_handler() -> impl IntoResponse {
    let guard = TOKEN_DRIFT_STATE.read();
    let entries: Vec<TierTokenDrift> = match guard.as_ref() {
        Some(state) => state
            .iter()
            .map(|(tier, e)| {
                let cum_drift = if e.upstream_sum > 0 {
                    ((e.local_sum as f64 - e.upstream_sum as f64) / e.upstream_sum as f64) * 100.0
                } else {
                    0.0
                };
                TierTokenDrift {
                    tier: tier.clone(),
                    samples: e.samples,
                    cumulative_local: e.local_sum,
                    cumulative_upstream: e.upstream_sum,
                    cumulative_drift_pct: (cum_drift * 10.0).round() / 10.0,
                    last_local: e.last_local,
                    last_upstream: e.last_upstream,
                    last_drift_pct: (e.last_drift_pct * 10.0).round() / 10.0,
                }
            })
            .collect(),
        None => Vec::new(),
    };
    Json(entries)
}

/// Handler for GET /v1/token-audit - returns the most recent pre-request token
/// audit entries from the in-memory ring buffer. Each entry captures the
/// per-component token estimate (messages, system, tools) computed before a
/// request is dispatched to a backend tier.
pub async fn token_audit_handler() -> impl IntoResponse {
    let guard = AUDIT_LOG.read();
    let entries: Vec<PreRequestAuditEntry> = match guard.as_ref() {
        Some(log) => log.iter().cloned().collect(),
        None => Vec::new(),
    };
    Json(entries)
}

/// Aggregated usage summary returned by /v1/usage.
#[derive(Debug, Serialize)]
pub struct UsageSummary {
    pub total_requests: u64,
    pub total_failures: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub active_streams: f64,
    pub tiers: Vec<TierUsage>,
}

#[derive(Debug, Serialize)]
pub struct TierUsage {
    pub tier: String,
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub avg_duration_seconds: f64,
}

/// Handler for GET /v1/usage - returns JSON usage summary.
pub async fn usage_handler() -> impl IntoResponse {
    let mut tiers: std::collections::HashMap<String, TierUsage> = std::collections::HashMap::new();

    // Collect per-tier request counts
    let req_metrics: Vec<prometheus::proto::MetricFamily> = REQUESTS_TOTAL.collect();
    for mf in &req_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "tier" {
                    let tier = label.get_value().to_string();
                    let entry = tiers.entry(tier.clone()).or_insert_with(|| TierUsage {
                        tier,
                        requests: 0,
                        failures: 0,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_read_tokens: 0,
                        cache_creation_tokens: 0,
                        avg_duration_seconds: 0.0,
                    });
                    entry.requests = m.get_counter().get_value() as u64;
                }
            }
        }
    }

    // Collect per-tier failure counts
    let fail_metrics: Vec<prometheus::proto::MetricFamily> = FAILURES_TOTAL.collect();
    for mf in &fail_metrics {
        for m in mf.get_metric() {
            let mut tier_name = String::new();
            for label in m.get_label() {
                if label.get_name() == "tier" {
                    tier_name = label.get_value().to_string();
                }
            }
            if !tier_name.is_empty() {
                let entry = tiers.entry(tier_name.clone()).or_insert_with(|| TierUsage {
                    tier: tier_name,
                    requests: 0,
                    failures: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    avg_duration_seconds: 0.0,
                });
                entry.failures += m.get_counter().get_value() as u64;
            }
        }
    }

    // Collect per-tier input tokens
    let input_metrics: Vec<prometheus::proto::MetricFamily> = INPUT_TOKENS_TOTAL.collect();
    for mf in &input_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "tier" {
                    let tier = label.get_value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        entry.input_tokens = m.get_counter().get_value() as u64;
                    }
                }
            }
        }
    }

    // Collect per-tier output tokens
    let output_metrics: Vec<prometheus::proto::MetricFamily> = OUTPUT_TOKENS_TOTAL.collect();
    for mf in &output_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "tier" {
                    let tier = label.get_value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        entry.output_tokens = m.get_counter().get_value() as u64;
                    }
                }
            }
        }
    }

    // Collect cache read tokens
    let cache_read_metrics: Vec<prometheus::proto::MetricFamily> = CACHE_READ_TOKENS_TOTAL.collect();
    for mf in &cache_read_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "tier" {
                    let tier = label.get_value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        entry.cache_read_tokens = m.get_counter().get_value() as u64;
                    }
                }
            }
        }
    }

    // Collect cache creation tokens
    let cache_create_metrics: Vec<prometheus::proto::MetricFamily> = CACHE_CREATION_TOKENS_TOTAL.collect();
    for mf in &cache_create_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "tier" {
                    let tier = label.get_value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        entry.cache_creation_tokens = m.get_counter().get_value() as u64;
                    }
                }
            }
        }
    }

    // Collect avg durations
    let dur_metrics: Vec<prometheus::proto::MetricFamily> = REQUEST_DURATION.collect();
    for mf in &dur_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "tier" {
                    let tier = label.get_value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        let h = m.get_histogram();
                        let count = h.get_sample_count();
                        if count > 0 {
                            entry.avg_duration_seconds = h.get_sample_sum() / count as f64;
                        }
                    }
                }
            }
        }
    }

    let mut tier_list: Vec<TierUsage> = tiers.into_values().collect();
    tier_list.sort_by(|a, b| a.tier.cmp(&b.tier));

    let summary = UsageSummary {
        total_requests: TOTAL_REQUESTS.load(Ordering::Relaxed),
        total_failures: TOTAL_FAILURES.load(Ordering::Relaxed),
        total_input_tokens: TOTAL_INPUT_TOKENS.load(Ordering::Relaxed),
        total_output_tokens: TOTAL_OUTPUT_TOKENS.load(Ordering::Relaxed),
        active_streams: ACTIVE_STREAMS.get(),
        tiers: tier_list,
    };

    Json(summary)
}

pub async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = vec![];
    encoder.encode(&metric_families, &mut buffer).unwrap();

    (
        [("content-type", "text/plain; version=0.0.4")],
        buffer,
    )
}

/// Per-tier latency entry for the /v1/latencies JSON endpoint.
#[derive(Debug, Serialize)]
pub struct TierLatency {
    pub tier: String,
    pub ewma_seconds: f64,
    pub sample_count: u64,
}

/// Handler for GET /v1/latencies - returns per-tier EWMA latencies as JSON.
/// Reads from the shared EwmaTracker in AppState.
pub fn get_latency_entries(tracker: &EwmaTracker) -> Vec<TierLatency> {
    tracker
        .get_all_latencies()
        .into_iter()
        .map(|(tier, ewma, count)| TierLatency {
            tier,
            ewma_seconds: ewma,
            sample_count: count,
        })
        .collect()
}
