use axum::response::IntoResponse;
use axum::Json;
use lazy_static::lazy_static;
use prometheus::{
    register_counter_vec, register_gauge, register_gauge_vec, register_histogram_vec, CounterVec,
    Encoder, Gauge, GaugeVec, HistogramVec, TextEncoder,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

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
}

// Per-tier EWMA latency state: (ewma_seconds, sample_count)
static TIER_LATENCY_STATE: RwLock<Option<HashMap<String, (f64, u64)>>> = RwLock::new(None);

const EWMA_ALPHA: f64 = 0.3;

// Atomic counters for fast aggregate access without Prometheus iteration
static TOTAL_INPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
static TOTAL_OUTPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
static TOTAL_REQUESTS: AtomicU64 = AtomicU64::new(0);
static TOTAL_FAILURES: AtomicU64 = AtomicU64::new(0);

pub fn record_request(tier: &str) {
    REQUESTS_TOTAL.with_label_values(&[tier]).inc();
    TOTAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_request_duration(tier: &str, duration: f64) {
    REQUEST_DURATION.with_label_values(&[tier]).observe(duration);
    record_tier_latency(tier, duration);
}

/// Update EWMA latency for a tier. Called on every successful request.
fn record_tier_latency(tier: &str, duration: f64) {
    let mut guard = TIER_LATENCY_STATE.write().unwrap();
    let state = guard.get_or_insert_with(HashMap::new);
    let entry = state.entry(tier.to_string()).or_insert((0.0, 0));
    if entry.1 == 0 {
        // First sample: initialize directly
        entry.0 = duration;
    } else {
        // EWMA: new = alpha * sample + (1 - alpha) * old
        entry.0 = EWMA_ALPHA * duration + (1.0 - EWMA_ALPHA) * entry.0;
    }
    entry.1 += 1;
    TIER_EWMA_LATENCY
        .with_label_values(&[tier])
        .set(entry.0);
}

/// Get current EWMA latencies for all tiers. Returns (tier_name, ewma_seconds, sample_count).
pub fn get_tier_latencies() -> Vec<(String, f64, u64)> {
    let guard = TIER_LATENCY_STATE.read().unwrap();
    match guard.as_ref() {
        Some(state) => state
            .iter()
            .map(|(k, (ewma, count))| (k.clone(), *ewma, *count))
            .collect(),
        None => Vec::new(),
    }
}

pub fn record_failure(tier: &str, reason: &str) {
    FAILURES_TOTAL.with_label_values(&[tier, reason]).inc();
    TOTAL_FAILURES.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_active_streams(delta: i64) {
    if delta > 0 {
        ACTIVE_STREAMS.add(delta as f64);
    } else {
        ACTIVE_STREAMS.sub((-delta) as f64);
    }
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
    let req_metrics = REQUESTS_TOTAL.collect();
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
    let fail_metrics = FAILURES_TOTAL.collect();
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
    let input_metrics = INPUT_TOKENS_TOTAL.collect();
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
    let output_metrics = OUTPUT_TOKENS_TOTAL.collect();
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
    let cache_read_metrics = CACHE_READ_TOKENS_TOTAL.collect();
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
    let cache_create_metrics = CACHE_CREATION_TOKENS_TOTAL.collect();
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
    let dur_metrics = REQUEST_DURATION.collect();
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
pub async fn latencies_handler() -> impl IntoResponse {
    let entries: Vec<TierLatency> = get_tier_latencies()
        .into_iter()
        .map(|(tier, ewma, count)| TierLatency {
            tier,
            ewma_seconds: ewma,
            sample_count: count,
        })
        .collect();
    Json(entries)
}
