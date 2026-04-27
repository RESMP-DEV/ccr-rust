// SPDX-License-Identifier: AGPL-3.0-or-later
// Metrics HTTP handler endpoints.
//
// These handlers serve JSON and Prometheus-text responses for
// /v1/usage, /v1/token-drift, /v1/token-audit, /v1/frontend-metrics,
// /v1/throughput, /metrics, and /v1/latencies.

use axum::response::IntoResponse;
use axum::Json;
use prometheus::core::Collector;
use tracing::debug;
use prometheus::{Encoder, TextEncoder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use crate::routing::EwmaTracker;

use super::{
    get_hist_offset, get_throughput_state, merge_histogram_offsets, PreRequestAuditEntry,
    ThroughputSample, ACTIVE_REQUESTS, ACTIVE_STREAMS, AUDIT_LOG, CACHE_CREATION_TOKENS_TOTAL,
    CACHE_READ_TOKENS_TOTAL, FAILURES_TOTAL, FRONTEND_REQUESTS_TOTAL, FRONTEND_REQUEST_LATENCY,
    INPUT_TOKENS_TOTAL, METRIC_FRONTEND_REQUEST_DURATION_SECONDS, METRIC_OUTPUT_TOKENS_PER_SECOND,
    METRIC_REQUEST_DURATION_SECONDS, METRIC_TTFT_SECONDS, OUTPUT_TOKENS_PER_SECOND,
    OUTPUT_TOKENS_TOTAL, REQUESTS_TOTAL, REQUEST_DURATION, TOKEN_DRIFT_STATE, TOTAL_FAILURES,
    TOTAL_INPUT_TOKENS, TOTAL_OUTPUT_TOKENS, TOTAL_REQUESTS, TTFT_SECONDS,
};

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
/// audit entries from the in-memory ring buffer.
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
    pub active_requests: f64,
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
    debug!("usage_handler called");
    let mut tiers: HashMap<String, TierUsage> = HashMap::new();

    // Collect per-tier request counts
    let req_metrics: Vec<prometheus::proto::MetricFamily> = REQUESTS_TOTAL.collect();
    for mf in &req_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.name() == "tier" {
                    let tier = label.value().to_string();
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
                    entry.requests = m.get_counter().value() as u64;
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
                if label.name() == "tier" {
                    tier_name = label.value().to_string();
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
                entry.failures += m.get_counter().value() as u64;
            }
        }
    }

    // Collect per-tier input tokens
    let input_metrics: Vec<prometheus::proto::MetricFamily> = INPUT_TOKENS_TOTAL.collect();
    for mf in &input_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.name() == "tier" {
                    let tier = label.value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        entry.input_tokens = m.get_counter().value() as u64;
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
                if label.name() == "tier" {
                    let tier = label.value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        entry.output_tokens = m.get_counter().value() as u64;
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
                if label.name() == "tier" {
                    let tier = label.value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        entry.cache_read_tokens = m.get_counter().value() as u64;
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
                if label.name() == "tier" {
                    let tier = label.value().to_string();
                    if let Some(entry) = tiers.get_mut(&tier) {
                        entry.cache_creation_tokens = m.get_counter().value() as u64;
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
                if label.name() == "tier" {
                    let tier = label.value().to_string();
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
        active_requests: ACTIVE_REQUESTS.get(),
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
                if label.name() == "frontend" {
                    let frontend = label.value().to_string();
                    let entry = frontend_metrics.entry(frontend.clone()).or_insert_with(|| {
                        FrontendMetrics {
                            frontend,
                            requests: 0,
                            avg_latency_ms: 0.0,
                        }
                    });
                    entry.requests = m.get_counter().value() as u64;
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
                if label.name() == "frontend" {
                    let frontend = label.value().to_string();
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

/// Per-tier throughput summary returned by /v1/throughput.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TierThroughput {
    pub tier: String,
    /// Number of completed streaming requests with throughput data.
    pub samples: u64,
    /// Average TTFT (time to first token) in seconds.
    pub avg_ttft_seconds: f64,
    /// p50 TTFT from Prometheus histogram (approximate).
    pub p50_ttft_seconds: f64,
    /// p90 TTFT from Prometheus histogram (approximate).
    pub p90_ttft_seconds: f64,
    /// p99 TTFT from Prometheus histogram (approximate).
    pub p99_ttft_seconds: f64,
    /// Average output throughput in tokens/sec.
    pub avg_tokens_per_second: f64,
    /// p50 tok/s from Prometheus histogram (approximate).
    pub p50_tokens_per_second: f64,
    /// Recent samples (last N per tier).
    pub recent: Vec<ThroughputSample>,
}

/// Extract approximate percentile from a Prometheus histogram.
/// Uses linear interpolation within buckets.
fn histogram_percentile(metric: &prometheus::proto::Metric, quantile: f64) -> f64 {
    let h = metric.get_histogram();
    let count = h.get_sample_count() as f64;
    if count == 0.0 {
        return 0.0;
    }
    let target = quantile * count;
    let buckets = h.get_bucket();
    let mut prev_count = 0.0;
    let mut prev_bound = 0.0;
    for bucket in buckets {
        let cum = bucket.cumulative_count() as f64;
        let bound = bucket.upper_bound();
        if cum >= target {
            // Linear interpolation within this bucket
            let bucket_count = cum - prev_count;
            if bucket_count > 0.0 {
                let fraction = (target - prev_count) / bucket_count;
                return prev_bound + fraction * (bound - prev_bound);
            }
            return bound;
        }
        prev_count = cum;
        prev_bound = bound;
    }
    h.get_sample_sum() / count // fallback: return mean
}

/// Handler for GET /v1/throughput - returns per-tier TTFT and tok/s metrics.
pub async fn throughput_handler() -> impl IntoResponse {
    debug!("throughput_handler called");
    let mut tiers: HashMap<String, TierThroughput> = HashMap::new();
    let throughput_state = get_throughput_state();

    // Collect TTFT histogram data
    let ttft_metrics: Vec<prometheus::proto::MetricFamily> = TTFT_SECONDS.collect();
    for mf in &ttft_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.name() == "tier" {
                    let tier = label.value().to_string();
                    let h = m.get_histogram();
                    let mut count = h.get_sample_count();
                    let mut sum = h.get_sample_sum();
                    if let Some(offset) = get_hist_offset(METRIC_TTFT_SECONDS, &[("tier", &tier)]) {
                        count += offset.sample_count;
                        sum += offset.sample_sum;
                    }
                    let recent = throughput_state
                        .get(&tier)
                        .map(|s| s.iter().cloned().collect())
                        .unwrap_or_default();
                    let entry = tiers.entry(tier.clone()).or_insert_with(|| TierThroughput {
                        tier,
                        samples: 0,
                        avg_ttft_seconds: 0.0,
                        p50_ttft_seconds: 0.0,
                        p90_ttft_seconds: 0.0,
                        p99_ttft_seconds: 0.0,
                        avg_tokens_per_second: 0.0,
                        p50_tokens_per_second: 0.0,
                        recent,
                    });
                    entry.samples = count;
                    if count > 0 {
                        entry.avg_ttft_seconds = (sum / count as f64 * 1000.0).round() / 1000.0;
                        entry.p50_ttft_seconds =
                            (histogram_percentile(m, 0.5) * 1000.0).round() / 1000.0;
                        entry.p90_ttft_seconds =
                            (histogram_percentile(m, 0.9) * 1000.0).round() / 1000.0;
                        entry.p99_ttft_seconds =
                            (histogram_percentile(m, 0.99) * 1000.0).round() / 1000.0;
                    }
                }
            }
        }
    }

    // Collect tok/s histogram data
    let tps_metrics: Vec<prometheus::proto::MetricFamily> = OUTPUT_TOKENS_PER_SECOND.collect();
    for mf in &tps_metrics {
        for m in mf.get_metric() {
            for label in m.get_label() {
                if label.name() == "tier" {
                    let tier = label.value().to_string();
                    let h = m.get_histogram();
                    let mut count = h.get_sample_count();
                    let mut sum = h.get_sample_sum();
                    if let Some(offset) =
                        get_hist_offset(METRIC_OUTPUT_TOKENS_PER_SECOND, &[("tier", &tier)])
                    {
                        count += offset.sample_count;
                        sum += offset.sample_sum;
                    }
                    let recent = throughput_state
                        .get(&tier)
                        .map(|s| s.iter().cloned().collect())
                        .unwrap_or_default();
                    let entry = tiers.entry(tier.clone()).or_insert_with(|| TierThroughput {
                        tier,
                        samples: 0,
                        avg_ttft_seconds: 0.0,
                        p50_ttft_seconds: 0.0,
                        p90_ttft_seconds: 0.0,
                        p99_ttft_seconds: 0.0,
                        avg_tokens_per_second: 0.0,
                        p50_tokens_per_second: 0.0,
                        recent,
                    });
                    if count > 0 {
                        entry.avg_tokens_per_second = (sum / count as f64 * 10.0).round() / 10.0;
                        entry.p50_tokens_per_second =
                            (histogram_percentile(m, 0.5) * 10.0).round() / 10.0;
                    }
                }
            }
        }
    }

    let mut tier_list: Vec<TierThroughput> = tiers.into_values().collect();
    tier_list.sort_by(|a, b| a.tier.cmp(&b.tier));

    Json(tier_list)
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
