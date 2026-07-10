// SPDX-License-Identifier: AGPL-3.0-or-later
// Redis persistence for metrics.
//
// Saves and restores Prometheus counters, gauges, histograms, token drift
// state, audit log, and EWMA latency state to/from Redis.

use anyhow::{anyhow, Context, Result};
use redis::Commands;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread;
use tracing::{info, warn};

use crate::config::{PersistenceConfig, PersistenceMode};
use crate::ratelimit::restore_rate_limit_backoff_counter;
use crate::routing::EwmaTracker;

use super::sync_ewma_gauge;
use super::{
    PreRequestAuditEntry, TokenDriftEntry, AUDIT_LOG, AUDIT_LOG_CAPACITY,
    CACHE_CREATION_TOKENS_TOTAL, CACHE_READ_TOKENS_TOTAL, COST_USD_TOTAL, FAILURES_TOTAL,
    FRONTEND_REQUESTS_TOTAL, INPUT_TOKENS_TOTAL, METRIC_CACHE_CREATION_TOKENS_TOTAL,
    METRIC_CACHE_READ_TOKENS_TOTAL, METRIC_COST_USD_TOTAL, METRIC_FAILURES_TOTAL,
    METRIC_FRONTEND_REQUESTS_TOTAL, METRIC_FRONTEND_REQUEST_DURATION_SECONDS,
    METRIC_INPUT_TOKENS_TOTAL, METRIC_OUTPUT_TOKENS_TOTAL, METRIC_PEAK_ACTIVE_STREAMS,
    METRIC_PRE_REQUEST_TOKENS, METRIC_PRE_REQUEST_TOKENS_TOTAL, METRIC_RATE_LIMIT_BACKOFFS_TOTAL,
    METRIC_RATE_LIMIT_HITS_TOTAL, METRIC_REJECTED_STREAMS_TOTAL, METRIC_REQUESTS_TOTAL,
    METRIC_REQUEST_DURATION_SECONDS, METRIC_STREAM_BACKPRESSURE_TOTAL,
    METRIC_TIER_EWMA_LATENCY_SECONDS, METRIC_TOKEN_DRIFT_ABSOLUTE, METRIC_TOKEN_DRIFT_ALERTS_TOTAL,
    METRIC_TOKEN_DRIFT_PCT, OUTPUT_TOKENS_TOTAL, PEAK_ACTIVE_STREAMS, PRE_REQUEST_TOKENS,
    PRE_REQUEST_TOKENS_BUCKETS, RATE_LIMIT_HITS, REJECTED_STREAMS, REQUESTS_TOTAL,
    REQUEST_DURATION_BUCKETS, STREAM_BACKPRESSURE, TIER_EWMA_LATENCY, TOKEN_DRIFT_ABS,
    TOKEN_DRIFT_ALERTS, TOKEN_DRIFT_PCT, TOKEN_DRIFT_STATE, TOTAL_FAILURES, TOTAL_INPUT_TOKENS,
    TOTAL_OUTPUT_TOKENS, TOTAL_REQUESTS,
};

static REDIS_RUNTIME: OnceLock<RedisRuntime> = OnceLock::new();

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
pub(super) struct HistogramOffset {
    pub(super) sample_sum: f64,
    pub(super) sample_count: u64,
    cumulative_buckets: HashMap<String, u64>,
}

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

/// Delete all Redis keys belonging to the configured CCR persistence prefix.
///
/// This is intentionally prefix-scoped so it only removes CCR persistence
/// records, not unrelated Redis data.
pub fn clear_redis_persistence(redis_url: &str, prefix: &str) -> Result<usize> {
    let client = redis::Client::open(redis_url)
        .with_context(|| format!("Failed to create Redis client for {}", redis_url))?;
    let mut conn = client
        .get_connection()
        .context("Failed to connect to Redis for persistence cleanup")?;
    clear_redis_persistence_with_conn(&mut conn, prefix)
}

fn clear_redis_persistence_with_conn(conn: &mut redis::Connection, prefix: &str) -> Result<usize> {
    let mut cursor: u64 = 0;
    let mut deleted: usize = 0;
    loop {
        let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("COUNT")
            .arg(256)
            .query(conn)?;

        let keys_to_delete: Vec<String> = keys
            .into_iter()
            .filter(|key| key_matches_persistence_prefix(key, prefix))
            .collect();

        if !keys_to_delete.is_empty() {
            let removed: usize = redis::cmd("DEL").arg(&keys_to_delete).query(conn)?;
            deleted += removed;
        }

        cursor = next_cursor;
        if cursor == 0 {
            break;
        }
    }
    Ok(deleted)
}

pub(super) fn key_matches_persistence_prefix(key: &str, prefix: &str) -> bool {
    if key == prefix {
        return true;
    }
    key.strip_prefix(prefix)
        .map(|suffix| suffix.starts_with(':'))
        .unwrap_or(false)
}

fn redis_runtime() -> Option<&'static RedisRuntime> {
    REDIS_RUNTIME.get()
}

pub(super) fn persist_counter_inc(metric: &'static str, labels: &[(&str, &str)], by: f64) {
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

pub(super) fn persist_gauge_set(metric: &'static str, labels: &[(&str, &str)], value: f64) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::GaugeSet {
            metric,
            labels: encode_labels(labels),
            value,
        });
    }
}

pub(super) fn persist_gauge_max(metric: &'static str, labels: &[(&str, &str)], value: f64) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::GaugeMax {
            metric,
            labels: encode_labels(labels),
            value,
        });
    }
}

pub(super) fn persist_histogram_observe(metric: &'static str, labels: &[(&str, &str)], value: f64) {
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

pub(super) fn persist_token_drift_state(tier: &str, entry: &TokenDriftEntry) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::TokenDriftStateSet {
            tier: tier.to_string(),
            entry: entry.clone(),
        });
    }
}

pub(super) fn persist_token_audit(entry: &PreRequestAuditEntry) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::TokenAuditPush {
            entry: entry.clone(),
        });
    }
}

pub(super) fn persist_ewma_state(tier: &str, ewma: f64, samples: u64) {
    if let Some(runtime) = redis_runtime() {
        let _ = runtime.sender.send(PersistEvent::EwmaStateSet {
            tier: tier.to_string(),
            ewma,
            samples,
        });
    }
}

pub(super) fn get_hist_offset(
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
        METRIC_COST_USD_TOTAL,
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
        METRIC_COST_USD_TOTAL => {
            if let Some(tier) = get_label(&labels, "tier") {
                COST_USD_TOTAL.with_label_values(&[tier]).inc_by(value);
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
        metric.label.push(pair);
    }
    metric
}

fn encode_metric_labels(metric: &prometheus::proto::Metric) -> String {
    let mut labels = BTreeMap::new();
    for label in metric.get_label() {
        labels.insert(label.name().to_string(), label.value().to_string());
    }
    serde_json::to_string(&labels).unwrap_or_else(|_| "{}".to_string())
}

pub(super) fn merge_histogram_offsets(metric_families: &mut Vec<prometheus::proto::MetricFamily>) {
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
            .position(|family| family.name() == metric_name);

        if let Some(idx) = family_idx {
            let family = &mut metric_families[idx];
            let mut existing_indices: HashMap<String, usize> = HashMap::new();
            for (i, metric) in family.get_metric().iter().enumerate() {
                existing_indices.insert(encode_metric_labels(metric), i);
            }

            for (labels, offset) in offsets {
                if let Some(&metric_idx) = existing_indices.get(labels) {
                    let metric = &mut family.metric[metric_idx];
                    let hist = metric.histogram.mut_or_insert_default();
                    hist.set_sample_sum(hist.get_sample_sum() + offset.sample_sum);
                    hist.set_sample_count(hist.get_sample_count() + offset.sample_count);

                    let mut current_buckets: HashMap<String, u64> = HashMap::new();
                    for bucket in hist.get_bucket() {
                        current_buckets.insert(
                            format_bound(bucket.upper_bound()),
                            bucket.cumulative_count(),
                        );
                    }
                    for (bound, count) in &offset.cumulative_buckets {
                        *current_buckets.entry(bound.clone()).or_insert(0) += count;
                    }

                    hist.bucket.clear();
                    if let Some(bounds) = histogram_bounds(metric_name) {
                        for bound in bounds {
                            let key = format_bound(*bound);
                            let mut bucket = prometheus::proto::Bucket::new();
                            bucket.set_upper_bound(*bound);
                            bucket.set_cumulative_count(
                                current_buckets.get(&key).copied().unwrap_or(0),
                            );
                            hist.bucket.push(bucket);
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
                            hist.bucket.push(bucket);
                        }
                    }
                    metric.set_histogram(hist);
                    family.metric.push(metric);
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
                        hist.bucket.push(bucket);
                    }
                }
                metric.set_histogram(hist);
                family.metric.push(metric);
            }
            metric_families.push(family);
        }
    }
}
