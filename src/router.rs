use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::metrics::{
    get_tier_latencies, record_failure, record_request, record_request_duration, record_usage,
};
use crate::sse::stream_response;

const MAX_RETRIES: usize = 3;
/// Minimum samples before a tier's EWMA is trusted for routing decisions.
const MIN_LATENCY_SAMPLES: u64 = 3;

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

/// Reorder tiers by EWMA latency (lowest first). Tiers without enough samples
/// keep their original config-defined position.
fn latency_sorted_tiers(tiers: &[String]) -> Vec<(String, String)> {
    let latencies = get_tier_latencies();
    let latency_map: std::collections::HashMap<String, (f64, u64)> = latencies
        .into_iter()
        .map(|(tier, ewma, count)| (tier, (ewma, count)))
        .collect();

    // Build (tier_route, tier_name, ewma_or_none) tuples
    let mut entries: Vec<(usize, String, String, Option<f64>)> = tiers
        .iter()
        .enumerate()
        .map(|(idx, tier)| {
            let tier_name = format!("tier-{}", idx);
            let ewma = latency_map
                .get(&tier_name)
                .and_then(|(e, c)| if *c >= MIN_LATENCY_SAMPLES { Some(*e) } else { None });
            (idx, tier.clone(), tier_name, ewma)
        })
        .collect();

    // Stable sort: tiers with latency data sort by EWMA ascending,
    // tiers without data keep their original order but come after measured tiers.
    entries.sort_by(|a, b| {
        match (a.3, b.3) {
            (Some(la), Some(lb)) => la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0), // preserve config order
        }
    });

    entries.into_iter().map(|(_, tier, name, _)| (tier, name)).collect()
}

pub async fn handle_messages(
    State(config): State<Config>,
    Json(mut request): Json<AnthropicRequest>,
) -> Response {
    let start = std::time::Instant::now();
    let tiers = config.backend_tiers();

    info!("Incoming request for model: {}", request.model);

    // Sort tiers by observed latency (lowest EWMA first)
    let ordered = latency_sorted_tiers(&tiers);

    // Try each tier with retries
    for (tier, tier_name) in ordered.iter() {

        for attempt in 0..MAX_RETRIES {
            info!("Trying {} ({}), attempt {}/{}", tier, tier_name, attempt + 1, MAX_RETRIES);

            // Override model with current tier
            request.model = tier.clone();

            match try_request(&config, &request, tier, &tier_name).await {
                Ok(response) => {
                    let duration = start.elapsed().as_secs_f64();
                    record_request(&tier_name);
                    record_request_duration(&tier_name, duration);
                    info!("Success on {} after {:.2}s", tier_name, duration);
                    return response;
                }
                Err(e) => {
                    warn!("Failed {} attempt {}: {}", tier_name, attempt + 1, e);
                    record_failure(&tier_name, "request_failed");

                    if attempt < MAX_RETRIES - 1 {
                        // Exponential backoff
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            100 * 2_u64.pow(attempt as u32),
                        ))
                        .await;
                    }
                }
            }
        }
    }

    // All tiers exhausted
    error!("All tiers exhausted after {} attempts", ordered.len() * MAX_RETRIES);
    let error_resp = ErrorResponse {
        error: "All backend tiers failed".to_string(),
        tier: "all".to_string(),
        attempts: ordered.len() * MAX_RETRIES,
    };

    (StatusCode::SERVICE_UNAVAILABLE, Json(error_resp)).into_response()
}

async fn try_request(
    config: &Config,
    request: &AnthropicRequest,
    tier: &str,
    tier_name: &str,
) -> anyhow::Result<Response> {
    let provider = config
        .resolve_provider(tier)
        .ok_or_else(|| anyhow::anyhow!("Provider not found for tier: {}", tier))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(config.api_timeout_ms))
        .build()?;

    let url = format!("{}/chat/completions", provider.api_base_url.trim_end_matches('/'));

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Authorization",
        format!("Bearer {}", provider.api_key).parse()?,
    );
    headers.insert("Content-Type", "application/json".parse()?);

    let resp = client
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
        Ok(stream_response(resp).await)
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
            }
        }

        // Inject usage header so the Python client can extract it without re-parsing
        let mut response = (StatusCode::OK, body).into_response();
        response.headers_mut().insert(
            "x-ccr-tier",
            tier_name.parse().unwrap_or_default(),
        );
        Ok(response)
    }
}
