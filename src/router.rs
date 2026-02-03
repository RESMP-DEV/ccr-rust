use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::metrics::{record_failure, record_request, record_request_duration};
use crate::sse::stream_response;

const MAX_RETRIES: usize = 3;

#[derive(Debug, Deserialize)]
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

pub async fn handle_messages(
    State(config): State<Config>,
    Json(mut request): Json<AnthropicRequest>,
) -> Response {
    let start = std::time::Instant::now();
    let tiers = config.backend_tiers();
    
    info!("Incoming request for model: {}", request.model);
    
    // Try each tier with retries
    for (tier_idx, tier) in tiers.iter().enumerate() {
        let tier_name = format!("tier-{}", tier_idx);
        
        for attempt in 0..MAX_RETRIES {
            info!("Trying {} ({}), attempt {}/{}", tier, tier_name, attempt + 1, MAX_RETRIES);
            
            // Override model with current tier
            request.model = tier.clone();
            
            match try_request(&config, &request, tier).await {
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
    error!("All tiers exhausted after {} attempts", tiers.len() * MAX_RETRIES);
    let error_resp = ErrorResponse {
        error: "All backend tiers failed".to_string(),
        tier: "all".to_string(),
        attempts: tiers.len() * MAX_RETRIES,
    };
    
    (StatusCode::SERVICE_UNAVAILABLE, Json(error_resp)).into_response()
}

async fn try_request(
    config: &Config,
    request: &AnthropicRequest,
    tier: &str,
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
        Ok((StatusCode::OK, body).into_response())
    }
}
