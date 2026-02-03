use axum::response::IntoResponse;
use lazy_static::lazy_static;
use prometheus::{
    register_counter_vec, register_gauge, register_histogram_vec, CounterVec, Encoder, Gauge,
    HistogramVec, TextEncoder,
};

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
}

pub fn record_request(tier: &str) {
    REQUESTS_TOTAL.with_label_values(&[tier]).inc();
}

pub fn record_request_duration(tier: &str, duration: f64) {
    REQUEST_DURATION.with_label_values(&[tier]).observe(duration);
}

pub fn record_failure(tier: &str, reason: &str) {
    FAILURES_TOTAL.with_label_values(&[tier, reason]).inc();
}

pub fn increment_active_streams(delta: i64) {
    if delta > 0 {
        ACTIVE_STREAMS.add(delta as f64);
    } else {
        ACTIVE_STREAMS.sub((-delta) as f64);
    }
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
