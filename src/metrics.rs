use anyhow::{anyhow, Context, Result};
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
use redis::Commands;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::SystemTime;
use tiktoken_rs::cl100k_base;
use tracing::{info, warn};

use crate::config::{PersistenceConfig, PersistenceMode};
use crate::frontend::FrontendType;
use crate::ratelimit::restore_rate_limit_backoff_counter;
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
const METRIC_PRE_REQUEST_TOKENS_TOTAL: &str = "ccr_pre_request_tokens_total";
const METRIC_PRE_REQUEST_TOKENS: &str = "ccr_pre_request_tokens";
const METRIC_RATE_LIMIT_HITS_TOTAL: &str = "ccr_rate_limit_hits_total";
const METRIC_RATE_LIMIT_BACKOFFS_TOTAL: &str = "ccr_rate_limit_backoffs_total";
const METRIC_TIER_EWMA_LATENCY_SECONDS: &str = "ccr_tier_ewma_latency_seconds";
const METRIC_TOKEN_DRIFT_ABSOLUTE: &str = "ccr_token_drift_absolute";
const METRIC_TOKEN_DRIFT_PCT: &str = "ccr_token_drift_pct";
const METRIC_TOKEN_DRIFT_ALERTS_TOTAL: &str = "ccr_token_drift_alerts_total";

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

static REDIS_RUNTIME: OnceLock<RedisRuntime> = OnceLock::new();

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
    /// Sum of all component token estimates.
    pub total_tokens: u64,
}

#[derive(Debug)]
struct RedisRuntime {
    sender: Sender<PersistEvent>,
    histogram_offsets: HistogramOffsetStore,
}

#[derive(Debug, Clone)]
enum PersistEvent {
    CounterInc {
        metric: &'static str,
        labels: String,
        by: f64,
    },
    GaugeSet {
        metric: &'static str,
        labels: String,
        value: f64,
    },
    GaugeMax {
        metric: &'static str,
        labels: String,
        value: f64,
    },
    HistogramObserve {
        metric: &'static str,
        labels: String,
        value: f64,
    },
    TokenDriftStateSet {
        tier: String,
        entry: TokenDriftEntry,
    },
    TokenAuditPush {
        entry: PreRequestAuditEntry,
    },
    EwmaStateSet {
        tier: String,
        ewma: f64,
        samples: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedEwmaState {
    ewma: f64,
    samples: u64,
}

#[derive(Debug, Clone, Default)]
struct RedisSnapshot {
    counters: HashMap<&'static str, HashMap<String, f64>>,
    gauges: HashMap<&'static str, HashMap<String, f64>>,
    histogram_offsets: HistogramOffsetStore,
    token_drift_state: HashMap<String, TokenDriftEntry>,
    token_audit_log: Vec<PreRequestAuditEntry>,
    ewma_state: HashMap<String, PersistedEwmaState>,
}

#[derive(Debug, Clone, Default)]
struct HistogramOffsetStore {
    by_metric: HashMap<&'static str, HashMap<String, HistogramOffset>>,
}

#[derive(Debug, Clone, Default)]
struct HistogramOffset {
    sample_sum: f64,
    sample_count: u64,
    cumulative_buckets: HashMap<String, u64>,
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

/// Initialize optional metrics persistence.
///
/// When `Persistence.mode = "redis"`, CCR-Rust restores counters/gauges,
/// token drift state, audit log, EWMA state, and histogram offsets from Redis.
pub fn init_persistence(config: &PersistenceConfig, ewma_tracker: &EwmaTracker) -> Result<()> {
    if config.mode != PersistenceMode::Redis {
        return Ok(());
    }
    if REDIS_RUNTIME.get().is_some() {
        return Ok(());
    }

    let redis_url = config
        .redis_url
        .clone()
        .or_else(|| std::env::var("CCR_REDIS_URL").ok())
        .ok_or_else(|| anyhow!("Persistence.mode=redis requires Persistence.redis_url"))?;
    let redis_prefix = config.redis_prefix.clone();

    let client = redis::Client::open(redis_url.as_str())
        .with_context(|| format!("Failed to create Redis client for {}", redis_url))?;
    let mut conn = client
        .get_connection()
        .context("Failed to connect to Redis for persistence snapshot load")?;
    let snapshot = load_snapshot(&mut conn, &redis_prefix)?;
    apply_snapshot(&snapshot, ewma_tracker);
    sync_ewma_gauge(ewma_tracker);

    let (tx, rx) = mpsc::channel();
    spawn_redis_worker(client, redis_prefix, rx);

    REDIS_RUNTIME
        .set(RedisRuntime {
            sender: tx,
            histogram_offsets: snapshot.histogram_offsets.clone(),
        })
        .map_err(|_| anyhow!("Redis runtime is already initialized"))?;

    info!("Redis metrics persistence initialized");
    Ok(())
}

fn redis_runtime() -> Option<&'static RedisRuntime> {
    REDIS_RUNTIME.get()
}

fn persist_counter_inc(metric: &'static str, labels: &[(&str, &str)], by: f64) {
    if by <= 0.0 {
        return;
    }
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::CounterInc {
            metric,
            labels: encode_labels(labels),
            by,
        });
    }
}

fn persist_gauge_set(metric: &'static str, labels: &[(&str, &str)], value: f64) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::GaugeSet {
            metric,
            labels: encode_labels(labels),
            value,
        });
    }
}

fn persist_gauge_max(metric: &'static str, labels: &[(&str, &str)], value: f64) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::GaugeMax {
            metric,
            labels: encode_labels(labels),
            value,
        });
    }
}

fn persist_histogram_observe(metric: &'static str, labels: &[(&str, &str)], value: f64) {
    if !value.is_finite() || value < 0.0 {
        return;
    }
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::HistogramObserve {
            metric,
            labels: encode_labels(labels),
            value,
        });
    }
}

fn persist_token_drift_state(tier: &str, entry: &TokenDriftEntry) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::TokenDriftStateSet {
            tier: tier.to_string(),
            entry: entry.clone(),
        });
    }
}

fn persist_token_audit(entry: &PreRequestAuditEntry) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::TokenAuditPush {
            entry: entry.clone(),
        });
    }
}

fn persist_ewma_state(tier: &str, ewma: f64, samples: u64) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::EwmaStateSet {
            tier: tier.to_string(),
            ewma,
            samples,
        });
    }
}

fn get_hist_offset(
    metric: &'static str,
    labels: &[(&str, &str)],
) -> Option<&'static HistogramOffset> {
    let runtime = redis_runtime()?;
    runtime
        .histogram_offsets
        .by_metric
        .get(metric)
        .and_then(|by_label| by_label.get(&encode_labels(labels)))
}

fn encode_labels(labels: &[(&str, &str)]) -> String {
    let map: BTreeMap<&str, &str> = labels.iter().copied().collect();
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
}

fn decode_labels(encoded: &str) -> Option<BTreeMap<String, String>> {
    serde_json::from_str::<BTreeMap<String, String>>(encoded).ok()
}

fn get_label<'a>(labels: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    labels.get(key).map(|s| s.as_str())
}

fn snapshot_counter_sum(snapshot: &RedisSnapshot, metric: &'static str) -> u64 {
    snapshot
        .counters
        .get(metric)
        .map(|m| m.values().copied().sum::<f64>().max(0.0) as u64)
        .unwrap_or(0)
}

fn load_snapshot(conn: &mut redis::Connection, prefix: &str) -> Result<RedisSnapshot> {
    let counter_metrics = [
        METRIC_REQUESTS_TOTAL,
        METRIC_FRONTEND_REQUESTS_TOTAL,
        METRIC_FAILURES_TOTAL,
        METRIC_INPUT_TOKENS_TOTAL,
        METRIC_OUTPUT_TOKENS_TOTAL,
        METRIC_CACHE_READ_TOKENS_TOTAL,
        METRIC_CACHE_CREATION_TOKENS_TOTAL,
        METRIC_STREAM_BACKPRESSURE_TOTAL,
        METRIC_REJECTED_STREAMS_TOTAL,
        METRIC_PRE_REQUEST_TOKENS_TOTAL,
        METRIC_RATE_LIMIT_HITS_TOTAL,
        METRIC_RATE_LIMIT_BACKOFFS_TOTAL,
        METRIC_TOKEN_DRIFT_ALERTS_TOTAL,
    ];
    let gauge_metrics = [
        METRIC_PEAK_ACTIVE_STREAMS,
        METRIC_TIER_EWMA_LATENCY_SECONDS,
        METRIC_TOKEN_DRIFT_ABSOLUTE,
        METRIC_TOKEN_DRIFT_PCT,
    ];
    let histogram_metrics = [
        METRIC_REQUEST_DURATION_SECONDS,
        METRIC_FRONTEND_REQUEST_DURATION_SECONDS,
        METRIC_PRE_REQUEST_TOKENS,
    ];

    let mut snapshot = RedisSnapshot::default();

    for metric in counter_metrics {
        let key = redis_counter_key(prefix, metric);
        let values: HashMap<String, f64> = conn.hgetall(&key).unwrap_or_default();
        snapshot.counters.insert(metric, values);
    }

    for metric in gauge_metrics {
        let key = redis_gauge_key(prefix, metric);
        let values: HashMap<String, f64> = conn.hgetall(&key).unwrap_or_default();
        snapshot.gauges.insert(metric, values);
    }

    for metric in histogram_metrics {
        let sums: HashMap<String, f64> = conn
            .hgetall(redis_hist_sum_key(prefix, metric))
            .unwrap_or_default();
        let counts: HashMap<String, u64> = conn
            .hgetall(redis_hist_count_key(prefix, metric))
            .unwrap_or_default();

        let mut by_label: HashMap<String, HistogramOffset> = HashMap::new();
        for (labels, sample_sum) in sums {
            let entry = by_label.entry(labels).or_default();
            entry.sample_sum = sample_sum;
        }
        for (labels, sample_count) in counts {
            let entry = by_label.entry(labels).or_default();
            entry.sample_count = sample_count;
        }

        if let Some(bounds) = histogram_bounds(metric) {
            for bound in bounds {
                let bound_key = format_bound(*bound);
                let values: HashMap<String, u64> = conn
                    .hgetall(redis_hist_bucket_key(prefix, metric, &bound_key))
                    .unwrap_or_default();
                for (labels, count) in values {
                    let entry = by_label.entry(labels).or_default();
                    entry.cumulative_buckets.insert(bound_key.clone(), count);
                }
            }
        }

        snapshot
            .histogram_offsets
            .by_metric
            .insert(metric, by_label);
    }

    let drift_raw: HashMap<String, String> = conn
        .hgetall(redis_token_drift_state_key(prefix))
        .unwrap_or_default();
    for (tier, raw) in drift_raw {
        if let Ok(entry) = serde_json::from_str::<TokenDriftEntry>(&raw) {
            snapshot.token_drift_state.insert(tier, entry);
        }
    }

    let audit_raw: Vec<String> = conn
        .lrange(
            redis_token_audit_list_key(prefix),
            0,
            AUDIT_LOG_CAPACITY as isize - 1,
        )
        .unwrap_or_default();
    for raw in audit_raw {
        if let Ok(entry) = serde_json::from_str::<PreRequestAuditEntry>(&raw) {
            snapshot.token_audit_log.push(entry);
        }
    }

    let ewma_raw: HashMap<String, String> = conn
        .hgetall(redis_ewma_state_key(prefix))
        .unwrap_or_default();
    for (tier, raw) in ewma_raw {
        if let Ok(state) = serde_json::from_str::<PersistedEwmaState>(&raw) {
            snapshot.ewma_state.insert(tier, state);
        }
    }

    Ok(snapshot)
}

fn apply_snapshot(snapshot: &RedisSnapshot, ewma_tracker: &EwmaTracker) {
    for (metric, values) in &snapshot.counters {
        for (encoded_labels, value) in values {
            apply_counter_restore(metric, encoded_labels, *value);
        }
    }

    for (metric, values) in &snapshot.gauges {
        for (encoded_labels, value) in values {
            apply_gauge_restore(metric, encoded_labels, *value);
        }
    }

    TOTAL_REQUESTS.store(
        snapshot_counter_sum(snapshot, METRIC_REQUESTS_TOTAL),
        Ordering::Relaxed,
    );
    TOTAL_FAILURES.store(
        snapshot_counter_sum(snapshot, METRIC_FAILURES_TOTAL),
        Ordering::Relaxed,
    );
    TOTAL_INPUT_TOKENS.store(
        snapshot_counter_sum(snapshot, METRIC_INPUT_TOKENS_TOTAL),
        Ordering::Relaxed,
    );
    TOTAL_OUTPUT_TOKENS.store(
        snapshot_counter_sum(snapshot, METRIC_OUTPUT_TOKENS_TOTAL),
        Ordering::Relaxed,
    );

    if snapshot.token_drift_state.is_empty() {
        *TOKEN_DRIFT_STATE.write() = None;
    } else {
        *TOKEN_DRIFT_STATE.write() = Some(snapshot.token_drift_state.clone());
    }

    if snapshot.token_audit_log.is_empty() {
        *AUDIT_LOG.write() = None;
    } else {
        let mut deque = VecDeque::with_capacity(AUDIT_LOG_CAPACITY);
        for entry in snapshot
            .token_audit_log
            .iter()
            .take(AUDIT_LOG_CAPACITY)
            .cloned()
        {
            deque.push_back(entry);
        }
        *AUDIT_LOG.write() = Some(deque);
    }

    for (tier, state) in &snapshot.ewma_state {
        ewma_tracker.restore_tier_state(tier, state.ewma, state.samples);
    }
}

fn apply_counter_restore(metric: &'static str, encoded_labels: &str, value: f64) {
    if value <= 0.0 {
        return;
    }
    let labels = decode_labels(encoded_labels).unwrap_or_default();
    match metric {
        METRIC_REQUESTS_TOTAL => {
            if let Some(tier) = get_label(&labels, "tier") {
                REQUESTS_TOTAL.with_label_values(&[tier]).inc_by(value);
            }
        }
        METRIC_FRONTEND_REQUESTS_TOTAL => {
            if let Some(frontend) = get_label(&labels, "frontend") {
                FRONTEND_REQUESTS_TOTAL
                    .with_label_values(&[frontend])
                    .inc_by(value);
            }
        }
        METRIC_FAILURES_TOTAL => {
            if let (Some(tier), Some(reason)) =
                (get_label(&labels, "tier"), get_label(&labels, "reason"))
            {
                FAILURES_TOTAL
                    .with_label_values(&[tier, reason])
                    .inc_by(value);
            }
        }
        METRIC_INPUT_TOKENS_TOTAL => {
            if let Some(tier) = get_label(&labels, "tier") {
                INPUT_TOKENS_TOTAL.with_label_values(&[tier]).inc_by(value);
            }
        }
        METRIC_OUTPUT_TOKENS_TOTAL => {
            if let Some(tier) = get_label(&labels, "tier") {
                OUTPUT_TOKENS_TOTAL.with_label_values(&[tier]).inc_by(value);
            }
        }
        METRIC_CACHE_READ_TOKENS_TOTAL => {
            if let Some(tier) = get_label(&labels, "tier") {
                CACHE_READ_TOKENS_TOTAL
                    .with_label_values(&[tier])
                    .inc_by(value);
            }
        }
        METRIC_CACHE_CREATION_TOKENS_TOTAL => {
            if let Some(tier) = get_label(&labels, "tier") {
                CACHE_CREATION_TOKENS_TOTAL
                    .with_label_values(&[tier])
                    .inc_by(value);
            }
        }
        METRIC_STREAM_BACKPRESSURE_TOTAL => {
            STREAM_BACKPRESSURE.inc_by(value);
        }
        METRIC_REJECTED_STREAMS_TOTAL => {
            REJECTED_STREAMS.inc_by(value);
        }
        METRIC_PRE_REQUEST_TOKENS_TOTAL => {
            if let (Some(tier), Some(component)) =
                (get_label(&labels, "tier"), get_label(&labels, "component"))
            {
                PRE_REQUEST_TOKENS
                    .with_label_values(&[tier, component])
                    .inc_by(value);
            }
        }
        METRIC_RATE_LIMIT_HITS_TOTAL => {
            if let Some(tier) = get_label(&labels, "tier") {
                RATE_LIMIT_HITS.with_label_values(&[tier]).inc_by(value);
            }
        }
        METRIC_RATE_LIMIT_BACKOFFS_TOTAL => {
            if let Some(tier) = get_label(&labels, "tier") {
                restore_rate_limit_backoff_counter(tier, value);
            }
        }
        METRIC_TOKEN_DRIFT_ALERTS_TOTAL => {
            if let (Some(tier), Some(severity)) =
                (get_label(&labels, "tier"), get_label(&labels, "severity"))
            {
                TOKEN_DRIFT_ALERTS
                    .with_label_values(&[tier, severity])
                    .inc_by(value);
            }
        }
        _ => {}
    }
}

fn apply_gauge_restore(metric: &'static str, encoded_labels: &str, value: f64) {
    let labels = decode_labels(encoded_labels).unwrap_or_default();
    match metric {
        METRIC_PEAK_ACTIVE_STREAMS => {
            if value > PEAK_ACTIVE_STREAMS.get() {
                PEAK_ACTIVE_STREAMS.set(value);
            }
        }
        METRIC_TIER_EWMA_LATENCY_SECONDS => {
            if let Some(tier) = get_label(&labels, "tier") {
                TIER_EWMA_LATENCY.with_label_values(&[tier]).set(value);
            }
        }
        METRIC_TOKEN_DRIFT_ABSOLUTE => {
            if let Some(tier) = get_label(&labels, "tier") {
                TOKEN_DRIFT_ABS.with_label_values(&[tier]).set(value);
            }
        }
        METRIC_TOKEN_DRIFT_PCT => {
            if let Some(tier) = get_label(&labels, "tier") {
                TOKEN_DRIFT_PCT.with_label_values(&[tier]).set(value);
            }
        }
        _ => {}
    }
}

fn redis_counter_key(prefix: &str, metric: &str) -> String {
    format!("{}:counter:{}", prefix, metric)
}

fn redis_gauge_key(prefix: &str, metric: &str) -> String {
    format!("{}:gauge:{}", prefix, metric)
}

fn redis_hist_sum_key(prefix: &str, metric: &str) -> String {
    format!("{}:hist:{}:sum", prefix, metric)
}

fn redis_hist_count_key(prefix: &str, metric: &str) -> String {
    format!("{}:hist:{}:count", prefix, metric)
}

fn redis_hist_bucket_key(prefix: &str, metric: &str, bound: &str) -> String {
    format!("{}:hist:{}:bucket:{}", prefix, metric, bound)
}

fn redis_token_drift_state_key(prefix: &str) -> String {
    format!("{}:state:token-drift", prefix)
}

fn redis_token_audit_list_key(prefix: &str) -> String {
    format!("{}:list:token-audit", prefix)
}

fn redis_ewma_state_key(prefix: &str) -> String {
    format!("{}:state:ewma", prefix)
}

fn histogram_bounds(metric: &str) -> Option<&'static [f64]> {
    match metric {
        METRIC_REQUEST_DURATION_SECONDS | METRIC_FRONTEND_REQUEST_DURATION_SECONDS => {
            Some(REQUEST_DURATION_BUCKETS)
        }
        METRIC_PRE_REQUEST_TOKENS => Some(PRE_REQUEST_TOKENS_BUCKETS),
        _ => None,
    }
}

fn format_bound(bound: f64) -> String {
    format!("{:.6}", bound)
}

fn spawn_redis_worker(client: redis::Client, prefix: String, rx: Receiver<PersistEvent>) {
    thread::spawn(move || {
        let mut conn: Option<redis::Connection> = None;
        while let Ok(event) = rx.recv() {
            if conn.is_none() {
                match client.get_connection() {
                    Ok(c) => conn = Some(c),
                    Err(err) => {
                        warn!(error = %err, "Failed to connect to Redis persistence backend");
                        continue;
                    }
                }
            }

            let Some(connection) = conn.as_mut() else {
                continue;
            };

            if let Err(err) = persist_event(connection, &prefix, event) {
                warn!(error = %err, "Redis persistence write failed");
                conn = None;
            }
        }
    });
}

fn persist_event(conn: &mut redis::Connection, prefix: &str, event: PersistEvent) -> Result<()> {
    match event {
        PersistEvent::CounterInc { metric, labels, by } => {
            let _: f64 = redis::cmd("HINCRBYFLOAT")
                .arg(redis_counter_key(prefix, metric))
                .arg(labels)
                .arg(by)
                .query(conn)?;
        }
        PersistEvent::GaugeSet {
            metric,
            labels,
            value,
        } => {
            let _: () = conn.hset(redis_gauge_key(prefix, metric), labels, value)?;
        }
        PersistEvent::GaugeMax {
            metric,
            labels,
            value,
        } => {
            let key = redis_gauge_key(prefix, metric);
            let current: Option<f64> = conn.hget(&key, &labels).ok();
            if current.unwrap_or(f64::NEG_INFINITY) < value {
                let _: () = conn.hset(key, labels, value)?;
            }
        }
        PersistEvent::HistogramObserve {
            metric,
            labels,
            value,
        } => {
            let mut pipe = redis::pipe();
            pipe.cmd("HINCRBYFLOAT")
                .arg(redis_hist_sum_key(prefix, metric))
                .arg(&labels)
                .arg(value)
                .ignore()
                .cmd("HINCRBY")
                .arg(redis_hist_count_key(prefix, metric))
                .arg(&labels)
                .arg(1)
                .ignore();

            if let Some(bounds) = histogram_bounds(metric) {
                for bound in bounds {
                    if value <= *bound {
                        pipe.cmd("HINCRBY")
                            .arg(redis_hist_bucket_key(prefix, metric, &format_bound(*bound)))
                            .arg(&labels)
                            .arg(1)
                            .ignore();
                    }
                }
            }
            let _: () = pipe.query(conn)?;
        }
        PersistEvent::TokenDriftStateSet { tier, entry } => {
            let raw = serde_json::to_string(&entry)?;
            let _: () = conn.hset(redis_token_drift_state_key(prefix), tier, raw)?;
        }
        PersistEvent::TokenAuditPush { entry } => {
            let raw = serde_json::to_string(&entry)?;
            let mut pipe = redis::pipe();
            pipe.cmd("RPUSH")
                .arg(redis_token_audit_list_key(prefix))
                .arg(raw)
                .ignore()
                .cmd("LTRIM")
                .arg(redis_token_audit_list_key(prefix))
                .arg(-(AUDIT_LOG_CAPACITY as isize))
                .arg(-1)
                .ignore();
            let _: () = pipe.query(conn)?;
        }
        PersistEvent::EwmaStateSet {
            tier,
            ewma,
            samples,
        } => {
            let raw = serde_json::to_string(&PersistedEwmaState { ewma, samples })?;
            let _: () = conn.hset(redis_ewma_state_key(prefix), tier, raw)?;
        }
    }
    Ok(())
}

fn make_metric_with_labels(encoded_labels: &str) -> prometheus::proto::Metric {
    let mut metric = prometheus::proto::Metric::new();
    let labels = decode_labels(encoded_labels).unwrap_or_default();
    for (name, value) in labels {
        let mut pair = prometheus::proto::LabelPair::new();
        pair.set_name(name);
        pair.set_value(value);
        metric.mut_label().push(pair);
    }
    metric
}

fn encode_metric_labels(metric: &prometheus::proto::Metric) -> String {
    let mut labels = BTreeMap::new();
    for label in metric.get_label() {
        labels.insert(label.get_name().to_string(), label.get_value().to_string());
    }
    serde_json::to_string(&labels).unwrap_or_else(|_| "{}".to_string())
}

fn merge_histogram_offsets(metric_families: &mut Vec<prometheus::proto::MetricFamily>) {
    let Some(runtime) = redis_runtime() else {
        return;
    };

    for metric_name in [
        METRIC_REQUEST_DURATION_SECONDS,
        METRIC_FRONTEND_REQUEST_DURATION_SECONDS,
        METRIC_PRE_REQUEST_TOKENS,
    ] {
        let Some(offsets) = runtime.histogram_offsets.by_metric.get(metric_name) else {
            continue;
        };
        if offsets.is_empty() {
            continue;
        }

        let family_idx = metric_families
            .iter()
            .position(|family| family.get_name() == metric_name);

        if let Some(idx) = family_idx {
            let family = &mut metric_families[idx];
            let mut existing_indices: HashMap<String, usize> = HashMap::new();
            for (i, metric) in family.get_metric().iter().enumerate() {
                existing_indices.insert(encode_metric_labels(metric), i);
            }

            for (labels, offset) in offsets {
                if let Some(&metric_idx) = existing_indices.get(labels) {
                    let metric = &mut family.mut_metric()[metric_idx];
                    let hist = metric.mut_histogram();
                    hist.set_sample_sum(hist.get_sample_sum() + offset.sample_sum);
                    hist.set_sample_count(hist.get_sample_count() + offset.sample_count);

                    let mut current_buckets: HashMap<String, u64> = HashMap::new();
                    for bucket in hist.get_bucket() {
                        current_buckets.insert(
                            format_bound(bucket.get_upper_bound()),
                            bucket.get_cumulative_count(),
                        );
                    }
                    for (bound, count) in &offset.cumulative_buckets {
                        *current_buckets.entry(bound.clone()).or_insert(0) += count;
                    }

                    hist.clear_bucket();
                    if let Some(bounds) = histogram_bounds(metric_name) {
                        for bound in bounds {
                            let key = format_bound(*bound);
                            let mut bucket = prometheus::proto::Bucket::new();
                            bucket.set_upper_bound(*bound);
                            bucket.set_cumulative_count(
                                current_buckets.get(&key).copied().unwrap_or(0),
                            );
                            hist.mut_bucket().push(bucket);
                        }
                    }
                } else {
                    let mut metric = make_metric_with_labels(labels);
                    let mut hist = prometheus::proto::Histogram::new();
                    hist.set_sample_sum(offset.sample_sum);
                    hist.set_sample_count(offset.sample_count);
                    if let Some(bounds) = histogram_bounds(metric_name) {
                        for bound in bounds {
                            let key = format_bound(*bound);
                            let mut bucket = prometheus::proto::Bucket::new();
                            bucket.set_upper_bound(*bound);
                            bucket.set_cumulative_count(
                                offset.cumulative_buckets.get(&key).copied().unwrap_or(0),
                            );
                            hist.mut_bucket().push(bucket);
                        }
                    }
                    metric.set_histogram(hist);
                    family.mut_metric().push(metric);
                }
            }
        } else {
            let mut family = prometheus::proto::MetricFamily::new();
            family.set_name(metric_name.to_string());
            family.set_help("restored histogram offsets".to_string());
            family.set_field_type(prometheus::proto::MetricType::HISTOGRAM);

            for (labels, offset) in offsets {
                let mut metric = make_metric_with_labels(labels);
                let mut hist = prometheus::proto::Histogram::new();
                hist.set_sample_sum(offset.sample_sum);
                hist.set_sample_count(offset.sample_count);
                if let Some(bounds) = histogram_bounds(metric_name) {
                    for bound in bounds {
                        let key = format_bound(*bound);
                        let mut bucket = prometheus::proto::Bucket::new();
                        bucket.set_upper_bound(*bound);
                        bucket.set_cumulative_count(
                            offset.cumulative_buckets.get(&key).copied().unwrap_or(0),
                        );
                        hist.mut_bucket().push(bucket);
                    }
                }
                metric.set_histogram(hist);
                family.mut_metric().push(metric);
            }
            metric_families.push(family);
        }
    }
}

/// Get the current number of active streams.
pub fn get_active_streams() -> f64 {
    ACTIVE_STREAMS.get()
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

/// Per-tier drift summary for the /v1/token-drift JSON endpoint.
#[derive(Debug, Serialize, Deserialize, Clone)]
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FrontendMetrics {
    pub frontend: String,
    pub requests: u64,
    pub avg_latency_ms: f64,
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
#[allow(dead_code)]
pub async fn token_audit_handler() -> impl IntoResponse {
    let guard = AUDIT_LOG.read();
    let entries: Vec<PreRequestAuditEntry> = match guard.as_ref() {
        Some(log) => log.iter().cloned().collect(),
        None => Vec::new(),
    };
    Json(entries)
}

/// Aggregated usage summary returned by /v1/usage.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UsageSummary {
    pub total_requests: u64,
    pub total_failures: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub active_streams: f64,
    pub tiers: Vec<TierUsage>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
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
    let cache_read_metrics: Vec<prometheus::proto::MetricFamily> =
        CACHE_READ_TOKENS_TOTAL.collect();
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
    let cache_create_metrics: Vec<prometheus::proto::MetricFamily> =
        CACHE_CREATION_TOKENS_TOTAL.collect();
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

    // Collect avg durations (live histogram + persisted offset histogram)
    let mut duration_processed_tiers: HashSet<String> = HashSet::new();
    let dur_metrics: Vec<prometheus::proto::MetricFamily> = REQUEST_DURATION.collect();
    for mf in &dur_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "tier" {
                    let tier = label.get_value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        let h = m.get_histogram();
                        let mut sample_sum = h.get_sample_sum();
                        let mut count = h.get_sample_count();
                        if let Some(offset) =
                            get_hist_offset(METRIC_REQUEST_DURATION_SECONDS, &[("tier", &tier)])
                        {
                            sample_sum += offset.sample_sum;
                            count += offset.sample_count;
                        }
                        if count > 0 {
                            entry.avg_duration_seconds = sample_sum / count as f64;
                        }
                        duration_processed_tiers.insert(tier);
                    }
                }
            }
        }
    }

    // Fill duration averages for tiers that only exist in restored histogram offsets.
    for entry in tiers.values_mut() {
        if duration_processed_tiers.contains(&entry.tier) {
            continue;
        }
        if let Some(offset) =
            get_hist_offset(METRIC_REQUEST_DURATION_SECONDS, &[("tier", &entry.tier)])
        {
            if offset.sample_count > 0 {
                entry.avg_duration_seconds = offset.sample_sum / offset.sample_count as f64;
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

pub async fn frontend_metrics_handler() -> impl IntoResponse {
    let mut frontend_metrics: HashMap<String, FrontendMetrics> = HashMap::new();

    // Collect request counts
    let req_metrics = FRONTEND_REQUESTS_TOTAL.collect();
    for mf in &req_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "frontend" {
                    let frontend = label.get_value().to_string();
                    let entry = frontend_metrics.entry(frontend.clone()).or_insert_with(|| {
                        FrontendMetrics {
                            frontend,
                            requests: 0,
                            avg_latency_ms: 0.0,
                        }
                    });
                    entry.requests = m.get_counter().get_value() as u64;
                }
            }
        }
    }

    // Collect latency info (live histogram + persisted offset histogram)
    let mut frontend_latency_processed: HashSet<String> = HashSet::new();
    let lat_metrics = FRONTEND_REQUEST_LATENCY.collect();
    for mf in &lat_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.get_name() == "frontend" {
                    let frontend = label.get_value().to_string();
                    if let Some(entry) = frontend_metrics.get_mut(&frontend) {
                        let h = m.get_histogram();
                        let mut sample_sum = h.get_sample_sum();
                        let mut count = h.get_sample_count();
                        if let Some(offset) = get_hist_offset(
                            METRIC_FRONTEND_REQUEST_DURATION_SECONDS,
                            &[("frontend", &frontend)],
                        ) {
                            sample_sum += offset.sample_sum;
                            count += offset.sample_count;
                        }
                        if count > 0 {
                            entry.avg_latency_ms = (sample_sum * 1000.0) / count as f64;
                        }
                        frontend_latency_processed.insert(frontend);
                    }
                }
            }
        }
    }

    for entry in frontend_metrics.values_mut() {
        if frontend_latency_processed.contains(&entry.frontend) {
            continue;
        }
        if let Some(offset) = get_hist_offset(
            METRIC_FRONTEND_REQUEST_DURATION_SECONDS,
            &[("frontend", &entry.frontend)],
        ) {
            if offset.sample_count > 0 {
                entry.avg_latency_ms = (offset.sample_sum * 1000.0) / offset.sample_count as f64;
            }
        }
    }

    let mut metrics_list: Vec<FrontendMetrics> = frontend_metrics.into_values().collect();
    metrics_list.sort_by(|a, b| a.frontend.cmp(&b.frontend));

    Json(metrics_list)
}

pub async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let mut metric_families = prometheus::gather();
    merge_histogram_offsets(&mut metric_families);
    let mut buffer = vec![];
    encoder.encode(&metric_families, &mut buffer).unwrap();

    ([("content-type", "text/plain; version=0.0.4")], buffer)
}

/// Per-tier latency entry for the /v1/latencies JSON endpoint.
#[derive(Debug, Serialize, Deserialize, Clone)]
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
