use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::metrics::{
    record_failure, record_pre_request_tokens, record_request,
    record_request_duration, record_usage, sync_ewma_gauge, verify_token_usage,
};
use crate::routing::{AttemptTimer, EwmaTracker};
use crate::sse::{stream_response, StreamVerifyCtx};

const MAX_RETRIES: usize = 3;

/// Shared application state threaded through Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub ewma_tracker: Arc<EwmaTracker>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<Message>,

    #[serde(default)]
    pub system: Option<serde_json::Value>,

    #[serde(default)]
    pub max_tokens: Option<u32>,

    #[serde(default)]
    pub temperature: Option<f32>,

    #[serde(default)]
    pub stream: Option<bool>,

    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub tier: String,
    pub attempts: usize,
}

/// Usage block from Anthropic-style API responses.
#[derive(Debug, Deserialize, Default)]
struct ResponseUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// Wrapper to extract the `usage` field from a response body.
#[derive(Debug, Deserialize)]
struct ResponseWithUsage {
    #[serde(default)]
    usage: Option<ResponseUsage>,
}

pub async fn handle_messages(
    State(state): State<AppState>,
    Json(mut request): Json<AnthropicRequest>,
) -> Response {
    let start = std::time::Instant::now();
    let config = &state.config;
    let tiers = config.backend_tiers();

    info!("Incoming request for model: {}", request.model);

    // Sort tiers by observed EWMA latency (lowest first). Tiers without
    // enough samples keep their config-defined order.
    let ordered = state.ewma_tracker.sort_tiers(&tiers);

    // Serialize messages to JSON values once for pre-request token audit
    let msg_values: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    let tool_values: Option<Vec<serde_json::Value>> = request
        .tools
        .as_ref()
        .map(|t| t.clone());

    // Try each tier with retries
    for (tier, tier_name) in ordered.iter() {
        // Pre-request token audit: estimate input tokens before dispatching
        let local_estimate = record_pre_request_tokens(
            tier_name,
            &msg_values,
            request.system.as_ref(),
            tool_values.as_deref(),
        );

        let retry_config = config.get_tier_retry(tier_name);
        let max_retries = retry_config.max_retries;

        for attempt in 0..max_retries {
            info!("Trying {} ({}), attempt {}/{}", tier, tier_name, attempt + 1, max_retries);

            // Override model with current tier
            request.model = tier.clone();

            // Start per-attempt latency timer for EWMA tracking
            let timer = AttemptTimer::start(&state.ewma_tracker, tier_name);

            match try_request(config, &request, tier, tier_name, local_estimate).await {
                Ok(response) => {
                    let attempt_duration = timer.finish_success();
                    let total_duration = start.elapsed().as_secs_f64();
                    record_request(tier_name);
                    record_request_duration(tier_name, total_duration);
                    sync_ewma_gauge(&state.ewma_tracker);
                    info!(
                        "Success on {} after {:.2}s (attempt {:.3}s)",
                        tier_name, total_duration, attempt_duration
                    );
                    return response;
                }
                Err(e) => {
                    timer.finish_failure();
                    sync_ewma_gauge(&state.ewma_tracker);
                    warn!("Failed {} attempt {}: {}", tier_name, attempt + 1, e);
                    record_failure(tier_name, "request_failed");

                    if attempt < max_retries - 1 {
                        // Get current EWMA for this tier for dynamic backoff scaling
                        let ewma = state.ewma_tracker.get_latency(tier_name).map(|(e, _)| e);
                        let backoff = retry_config.backoff_duration_with_ewma(attempt, ewma);
                        info!(
                            tier = tier_name,
                            attempt = attempt + 1,
                            backoff_ms = backoff.as_millis(),
                            ewma = ewma.map(|e| format!("{:.3}s", e)).unwrap_or_else(|| "N/A".to_string()),
                            "sleeping before retry"
                        );
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }
    }

    // All tiers exhausted
    let total_attempts = ordered.len() * MAX_RETRIES;
    error!("All tiers exhausted after {} tier(s)", ordered.len());
    let error_resp = ErrorResponse {
        error: "All backend tiers failed".to_string(),
        tier: "all".to_string(),
        attempts: total_attempts,
    };

    (StatusCode::SERVICE_UNAVAILABLE, Json(error_resp)).into_response()
}

async fn try_request(
    config: &Config,
    request: &AnthropicRequest,
    tier: &str,
    tier_name: &str,
    local_estimate: u64,
) -> anyhow::Result<Response> {
    let provider = config
        .resolve_provider(tier)
        .ok_or_else(|| anyhow::anyhow!("Provider not found for tier: {}", tier))?;

    let url = format!("{}/chat/completions", provider.api_base_url.trim_end_matches('/'));

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Authorization",
        format!("Bearer {}", provider.api_key).parse()?,
    );
    headers.insert("Content-Type", "application/json".parse()?);

    let resp = config
        .http_client()
        .post(&url)
        .headers(headers)
        .json(&request)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await?;
        anyhow::bail!("Provider returned {}: {}", status, body);
    }

    // Handle streaming vs non-streaming
    if request.stream.unwrap_or(false) {
        let ctx = StreamVerifyCtx {
            tier_name: tier_name.to_string(),
            local_estimate,
        };
        Ok(stream_response(resp, config.sse_buffer_size(), Some(ctx)).await)
    } else {
        let body = resp.bytes().await?;

        // Extract usage from response body for metrics (best-effort, non-blocking)
        if let Ok(parsed) = serde_json::from_slice::<ResponseWithUsage>(&body) {
            if let Some(usage) = parsed.usage {
                record_usage(
                    tier_name,
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_input_tokens,
                    usage.cache_creation_input_tokens,
                );
                // Verify local token estimate against upstream-reported input tokens
                verify_token_usage(tier_name, local_estimate, usage.input_tokens);
            }
        }

        // Inject usage header so the Python client can extract it without re-parsing
        let mut response = (StatusCode::OK, body).into_response();
        response.headers_mut().insert(
            "x-ccr-tier",
            tier_name.parse().unwrap_or(axum::http::HeaderValue::from_static("unknown")),
        );
        Ok(response)
    }
}
