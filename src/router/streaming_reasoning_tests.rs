// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reasoning and served-model accounting on the Anthropic streaming path.
//!
//! Split out of `streaming.rs` so that hotspot keeps its headroom; this is a
//! sibling child module of `streaming`, compiled only for `cargo test`.

use super::tests::tracked_context;
use super::*;

/// Counter value for one metric family and label set, read from the
/// default Prometheus registry.
fn counter_value(metric: &str, labels: &[(&str, &str)]) -> f64 {
    let families = prometheus::gather();
    families
        .iter()
        .find(|family| family.name() == metric)
        .and_then(|family| {
            family.get_metric().iter().find(|sample| {
                labels.iter().all(|(name, value)| {
                    sample
                        .get_label()
                        .iter()
                        .any(|label| label.name() == *name && label.value() == *value)
                })
            })
        })
        .map(|sample| sample.get_counter().value())
        .unwrap_or(0.0)
}

/// The Anthropic streaming path used to drop reasoning tokens entirely.
/// They now reach the metrics as a labeled subset of the output tokens,
/// together with the model that actually served the request.
#[tokio::test]
async fn anthropic_stream_records_reasoning_tokens_and_served_model() {
    let tracker = std::sync::Arc::new(crate::ratelimit::RateLimitTracker::new());
    let (mut context, _admission) = tracked_context(&tracker);
    context.tier_name = "reasoning-tier".to_string();
    let payload = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":20,\"reasoning_tokens\":6}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response =
        stream_anthropic_response_with_tracking(byte_stream, 8, context, TransformerChain::new())
            .await;
    let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024).await;

    assert_eq!(
        counter_value("ccr_reasoning_tokens_total", &[("tier", "reasoning-tier")]),
        6.0
    );
    assert_eq!(
        counter_value(
            "ccr_served_model_requests_total",
            &[("tier", "reasoning-tier"), ("served_model", "model-v1")]
        ),
        1.0
    );
}
