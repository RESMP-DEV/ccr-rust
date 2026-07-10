// SPDX-License-Identifier: AGPL-3.0-or-later
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use std::time::Duration;
use tracing::{trace, warn};

use super::streaming::{
    stream_anthropic_response_with_tracking, stream_response_translated, BoxByteStream,
};
use super::translate_request::translate_request_anthropic_to_openai;
use super::translate_response::{build_transformer_chain, translate_response_openai_to_anthropic};
use super::types::*;
use crate::config::{Config, ProviderProtocol};
use crate::debug_capture::{CaptureBuilder, DebugCapture};
use crate::metrics::{
    record_rate_limit_backoff, record_rate_limit_hit, record_usage, verify_token_usage,
};
use crate::ratelimit::RateLimitTracker;
use crate::sse::{SseFrameDecoder, StreamVerifyCtx};
use crate::transform::openai_to_anthropic::OpenAiToAnthropicTransformer;
use crate::transformer::{Transformer, TransformerChain, TransformerRegistry};
use futures::StreamExt;

fn sanitized_capture_headers(headers: &reqwest::header::HeaderMap) -> serde_json::Value {
    let mut values = serde_json::Map::new();
    for (name, value) in headers.iter().take(64) {
        let normalized = name.as_str().to_ascii_lowercase();
        let sensitive = matches!(
            normalized.as_str(),
            "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
        ) || normalized.contains("api-key")
            || normalized.contains("apikey")
            || normalized.contains("credential")
            || normalized.ends_with("-token")
            || normalized.ends_with("_token");
        let rendered = if sensitive {
            "[REDACTED]".to_string()
        } else {
            value
                .to_str()
                .unwrap_or("[NON_UTF8]")
                .chars()
                .take(512)
                .collect()
        };
        values.insert(
            name.as_str().to_string(),
            serde_json::Value::String(rendered),
        );
    }
    serde_json::Value::Object(values)
}

async fn persist_debug_capture(
    capture: Option<&Arc<DebugCapture>>,
    builder: Option<CaptureBuilder>,
    status: u16,
    response_body: &str,
    response_headers: Option<serde_json::Value>,
    error: Option<String>,
) {
    if let (Some(capture), Some(builder)) = (capture, builder) {
        let interaction = builder.complete(status, response_body, response_headers, error);
        if let Err(capture_err) = capture.record(interaction).await {
            warn!("Failed to record debug capture: {}", capture_err);
        }
    }
}

/// Peek at the first chunk of a streaming response to detect error payloads
/// wrapped in HTTP 200 (e.g. Z.AI returns quota errors as raw JSON in a
/// `text/event-stream` body).  Returns `Err` if an error is found, or a
/// `BoxByteStream` with the peeked bytes prepended for normal processing.
///
/// Uses a short timeout so that legitimate slow-start streams are not
/// penalised — error responses arrive near-instantly whereas real model
/// inference may take seconds for the first token.
///
/// Accumulates bytes in a loop (within the timeout window) to handle TCP
/// fragmentation — a single `resp.chunk()` may not contain a complete SSE
/// frame or JSON object.
const MAX_STREAM_PEEK_BYTES: usize = 1024 * 1024;

fn embedded_stream_error(payload: &str) -> Option<(String, String)> {
    let trimmed = payload.trim();
    let json_candidate = if trimmed.starts_with('{') {
        Some(trimmed.to_string())
    } else {
        trimmed
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .filter(|d| d.starts_with('{'))
            .map(|d| d.to_string())
    };

    let candidate = json_candidate?;
    let json = serde_json::from_str::<serde_json::Value>(&candidate).ok()?;
    json.get("error")?;

    let msg = json["error"]["message"]
        .as_str()
        .unwrap_or("Unknown error in stream body")
        .to_string();
    let code = json["error"]["code"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    Some((code, msg))
}

async fn check_stream_for_embedded_error(
    mut resp: reqwest::Response,
    tier_name: &str,
    first_event_timeout: Duration,
) -> Result<BoxByteStream, TryRequestError> {
    let deadline = tokio::time::Instant::now() + first_event_timeout;
    let mut buf = Vec::new();
    let mut decoder = SseFrameDecoder::new();

    loop {
        match tokio::time::timeout_at(deadline, resp.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                buf.extend_from_slice(&bytes);
                if buf.len() > MAX_STREAM_PEEK_BYTES {
                    return Err(TryRequestError::Other(anyhow::anyhow!(
                        "CCR stream first event peek exceeded {} bytes on {}",
                        MAX_STREAM_PEEK_BYTES,
                        tier_name
                    )));
                }
                let text = String::from_utf8_lossy(&buf);
                let trimmed = text.trim();

                if trimmed.starts_with('{') && trimmed.ends_with('}') {
                    break;
                }

                let mut saw_data_frame = false;
                for frame in decoder.push(&bytes) {
                    let data = frame.data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    saw_data_frame = true;
                    if let Some((code, msg)) = embedded_stream_error(data) {
                        warn!(
                            tier = tier_name,
                            error_code = code,
                            "Provider returned error in 200 stream body: {}",
                            msg
                        );
                        return Err(TryRequestError::Other(anyhow::anyhow!(
                            "Provider error in 200 stream (code {}): {}",
                            code,
                            msg
                        )));
                    }
                }
                if saw_data_frame {
                    break;
                }
            }
            Ok(Ok(None)) => {
                if buf.is_empty() {
                    let empty: BoxByteStream =
                        Box::pin(futures::stream::empty::<Result<bytes::Bytes, reqwest::Error>>());
                    return Ok(empty);
                }
                break;
            }
            Ok(Err(e)) => return Err(TryRequestError::Other(e.into())),
            Err(_timeout) => {
                return Err(TryRequestError::Other(anyhow::anyhow!(
                    "CCR stream first event timeout on {} after {}ms",
                    tier_name,
                    first_event_timeout.as_millis()
                )));
            }
        }
    }

    let text = String::from_utf8_lossy(&buf);
    if let Some((code, msg)) = embedded_stream_error(&text) {
        warn!(
            tier = tier_name,
            error_code = code,
            "Provider returned error in 200 stream body: {}",
            msg
        );
        return Err(TryRequestError::Other(anyhow::anyhow!(
            "Provider error in 200 stream (code {}): {}",
            code,
            msg
        )));
    }

    let peeked = bytes::Bytes::from(buf);
    let rest = resp.bytes_stream();
    let first = futures::stream::once(async move { Ok(peeked) });
    let stream: BoxByteStream = Box::pin(first.chain(rest));
    Ok(stream)
}

/// Check a non-streaming response body for an embedded error in a 200.
fn check_body_for_embedded_error(body: &[u8], tier_name: &str) -> Result<(), TryRequestError> {
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) {
        if json.get("error").is_some() {
            let msg = json["error"]["message"]
                .as_str()
                .unwrap_or("Unknown error in response body");
            let code = json["error"]["code"].as_str().unwrap_or("unknown");
            warn!(
                tier = tier_name,
                error_code = code,
                "Provider returned error in 200 body: {}",
                msg
            );
            return Err(TryRequestError::Other(anyhow::anyhow!(
                "Provider error in 200 body (code {}): {}",
                code,
                msg
            )));
        }
    }
    Ok(())
}

/// Extract rate limit information from upstream response headers.
pub(super) fn extract_rate_limit_headers(
    resp: &reqwest::Response,
) -> (Option<u32>, Option<std::time::Instant>) {
    let remaining = resp
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());

    let reset_at = resp
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|ts| {
            std::time::Instant::now()
                + std::time::Duration::from_secs(
                    ts.saturating_sub(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    ),
                )
        });

    (remaining, reset_at)
}

pub(super) struct TryRequestArgs<'a> {
    pub(super) config: &'a Config,
    pub(super) registry: &'a TransformerRegistry,
    pub(super) request: &'a AnthropicRequest,
    pub(super) tier: &'a str,
    pub(super) tier_name: &'a str,
    pub(super) local_estimate: u64,
    pub(super) stream_first_event_timeout: Duration,
    pub(super) stream_idle_timeout: Duration,
    pub(super) ratelimit_tracker: Arc<RateLimitTracker>,
    pub(super) debug_capture: Option<Arc<DebugCapture>>,
    /// Original OpenAI request body for passthrough to OpenAI-compatible backends.
    pub(super) openai_passthrough_body: Option<&'a serde_json::Value>,
}

pub(super) async fn try_request(args: TryRequestArgs<'_>) -> Result<Response, TryRequestError> {
    let TryRequestArgs {
        config,
        registry,
        request,
        tier,
        tier_name,
        local_estimate,
        stream_first_event_timeout,
        stream_idle_timeout,
        ratelimit_tracker,
        debug_capture,
        openai_passthrough_body,
    } = args;
    let provider = config.resolve_provider(tier).ok_or_else(|| {
        TryRequestError::Other(anyhow::anyhow!("Provider not found for tier: {}", tier))
    })?;

    // Build transformer chain from provider config
    let chain = build_transformer_chain(registry, provider, tier.split(',').nth(1).unwrap_or(tier));

    // Extract the actual model name from the tier (format: "provider,model")
    let model_name = tier.split(',').nth(1).unwrap_or(tier);

    // Apply request transformers if chain is not empty
    let transformed_request = if chain.is_empty() {
        serde_json::to_value(request).map_err(|e| TryRequestError::Other(e.into()))?
    } else {
        let req_value =
            serde_json::to_value(request).map_err(|e| TryRequestError::Other(e.into()))?;
        chain
            .apply_request(req_value)
            .map_err(TryRequestError::Other)?
    };

    // Only use passthrough when the chain has no transformers (transformers may
    // modify the Anthropic-shaped payload in ways we need to honour).
    let effective_passthrough = if chain.is_empty() {
        openai_passthrough_body.cloned()
    } else {
        None
    };

    match provider.protocol {
        ProviderProtocol::Openai => {
            try_request_via_openai_protocol(
                config,
                provider,
                TryRequestProtocolArgs {
                    transformed_request,
                    model_name,
                    tier_name,
                    local_estimate,
                    stream_first_event_timeout,
                    stream_idle_timeout,
                    ratelimit_tracker,
                    chain,
                    debug_capture,
                    openai_passthrough_body: effective_passthrough,
                },
            )
            .await
        }
        ProviderProtocol::Anthropic => {
            try_request_via_anthropic_protocol(
                config,
                provider,
                TryRequestProtocolArgs {
                    transformed_request,
                    model_name,
                    tier_name,
                    local_estimate,
                    stream_first_event_timeout,
                    stream_idle_timeout,
                    ratelimit_tracker,
                    chain,
                    debug_capture,
                    openai_passthrough_body: None,
                },
            )
            .await
        }
    }
}

pub(super) struct TryRequestProtocolArgs<'a> {
    pub(super) transformed_request: serde_json::Value,
    pub(super) model_name: &'a str,
    pub(super) tier_name: &'a str,
    pub(super) local_estimate: u64,
    pub(super) stream_first_event_timeout: Duration,
    pub(super) stream_idle_timeout: Duration,
    pub(super) ratelimit_tracker: Arc<RateLimitTracker>,
    pub(super) chain: TransformerChain,
    pub(super) debug_capture: Option<Arc<DebugCapture>>,
    /// Original OpenAI body for direct passthrough (skips Anthropic round-trip).
    pub(super) openai_passthrough_body: Option<serde_json::Value>,
}

pub(super) const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

pub(super) fn provider_endpoint_url(provider: &crate::config::Provider, endpoint: &str) -> String {
    let base = provider.api_base_url.trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    if base.ends_with(endpoint) {
        base.to_string()
    } else {
        format!("{}/{}", base, endpoint)
    }
}

pub(super) fn provider_openai_chat_completions_url(provider: &crate::config::Provider) -> String {
    provider_endpoint_url(provider, "chat/completions")
}

pub(super) fn provider_anthropic_messages_url(provider: &crate::config::Provider) -> String {
    provider_endpoint_url(provider, "messages")
}

pub(super) fn reqwest_status_to_axum(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
}

pub(super) fn insert_ccr_tier_header(response: &mut Response, tier_name: &str) {
    response.headers_mut().insert(
        "x-ccr-tier",
        tier_name
            .parse()
            .unwrap_or(axum::http::HeaderValue::from_static("unknown")),
    );
}

pub(super) fn build_openai_headers(
    provider: &crate::config::Provider,
) -> Result<reqwest::header::HeaderMap, TryRequestError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Authorization",
        format!("Bearer {}", provider.api_key).parse().map_err(
            |e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            },
        )?,
    );
    headers.insert(
        "Content-Type",
        "application/json"
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            })?,
    );

    // OpenRouter attribution headers for usage tracking
    // See: https://openrouter.ai/docs/api-reference/overview
    if provider.name.to_lowercase() == "openrouter"
        || provider.api_base_url.contains("openrouter.ai")
    {
        headers.insert(
            "HTTP-Referer",
            "https://github.com/RESMP-DEV/ccr-rust".parse().map_err(
                |e: reqwest::header::InvalidHeaderValue| {
                    TryRequestError::Other(anyhow::anyhow!("{}", e))
                },
            )?,
        );
        headers.insert(
            "X-Title",
            "ccr-rust"
                .parse()
                .map_err(|e: reqwest::header::InvalidHeaderValue| {
                    TryRequestError::Other(anyhow::anyhow!("{}", e))
                })?,
        );
    }

    // Merge provider-level extra headers (e.g., User-Agent for Kimi).
    if let Some(ref extra) = provider.extra_headers {
        for (key, value) in extra {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                value.parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(name, val);
            }
        }
    }

    Ok(headers)
}

pub(super) fn build_anthropic_headers(
    provider: &crate::config::Provider,
) -> Result<reqwest::header::HeaderMap, TryRequestError> {
    let mut headers = reqwest::header::HeaderMap::new();
    match provider
        .auth_header
        .as_deref()
        .unwrap_or("x-api-key")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "x-api-key" => {
            headers.insert(
                "x-api-key",
                provider
                    .api_key
                    .parse()
                    .map_err(|e: reqwest::header::InvalidHeaderValue| {
                        TryRequestError::Other(anyhow::anyhow!("{}", e))
                    })?,
            );
        }
        "authorization" | "bearer" | "authorization-bearer" => {
            headers.insert(
                "Authorization",
                format!("Bearer {}", provider.api_key).parse().map_err(
                    |e: reqwest::header::InvalidHeaderValue| {
                        TryRequestError::Other(anyhow::anyhow!("{}", e))
                    },
                )?,
            );
        }
        other => {
            return Err(TryRequestError::Other(anyhow::anyhow!(
                "Unsupported Anthropic auth_header '{}'",
                other
            )));
        }
    }
    headers.insert(
        "anthropic-version",
        provider
            .anthropic_version
            .as_deref()
            .unwrap_or(DEFAULT_ANTHROPIC_VERSION)
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            })?,
    );
    headers.insert(
        "Content-Type",
        "application/json"
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            })?,
    );

    // Merge provider-level extra headers (e.g., User-Agent for Kimi).
    if let Some(ref extra) = provider.extra_headers {
        for (key, value) in extra {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                value.parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(name, val);
            }
        }
    }

    Ok(headers)
}

pub(super) async fn try_request_via_openai_protocol(
    config: &Config,
    provider: &crate::config::Provider,
    args: TryRequestProtocolArgs<'_>,
) -> Result<Response, TryRequestError> {
    let TryRequestProtocolArgs {
        transformed_request,
        model_name,
        tier_name,
        local_estimate,
        stream_first_event_timeout,
        stream_idle_timeout,
        ratelimit_tracker,
        chain,
        debug_capture,
        openai_passthrough_body,
    } = args;

    let url = provider_openai_chat_completions_url(provider);
    let headers = build_openai_headers(provider)?;

    // Fast path: when the inbound request was already OpenAI-formatted (Codex
    // frontend) and no transformers need to modify it, reuse the original body
    // directly with only a model-name swap.  This eliminates the wasteful
    // OpenAI → Anthropic → deserialize → translate → OpenAI round-trip.
    let (openai_request_value, stream_flag) = if let Some(mut body) = openai_passthrough_body {
        // Swap model name to the backend's expected value.
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "model".to_string(),
                serde_json::Value::String(model_name.to_string()),
            );
        }
        let stream = body
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        trace!(tier = tier_name, model = model_name, url = %url, "OpenAI passthrough: sending original body directly");
        (body, stream)
    } else {
        // Legacy path: translate through Anthropic intermediate format.
        trace!(tier = tier_name, model = model_name, url = %url, "dispatching OpenAI-compatible upstream request");

        // Deserialize back to AnthropicRequest for translation.
        let request: AnthropicRequest = serde_json::from_value(transformed_request.clone())
            .map_err(|e| TryRequestError::Other(e.into()))?;

        // Translate Anthropic request to OpenAI format.
        let openai_request = translate_request_anthropic_to_openai(&request, model_name);
        let stream = request.stream.unwrap_or(false);
        let value =
            serde_json::to_value(&openai_request).map_err(|e| TryRequestError::Other(e.into()))?;
        (value, stream)
    };

    // Set up capture if enabled for this provider
    let capture_builder = if let Some(ref capture) = debug_capture {
        if capture.should_capture(&provider.name) {
            let mut builder = capture
                .builder(&provider.name, tier_name)
                .model(model_name)
                .url(&url)
                .request_body(openai_request_value.clone())
                .streaming(stream_flag);
            if capture.headers_enabled() {
                builder = builder.request_headers(sanitized_capture_headers(&headers));
            }
            Some(builder.start())
        } else {
            None
        }
    } else {
        None
    };

    let resp = config
        .http_client()
        .post(&url)
        .headers(headers)
        .json(&openai_request_value)
        .send()
        .await;

    // Handle connection errors with capture
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            // Record capture on connection failure
            if let (Some(builder), Some(capture)) = (capture_builder, debug_capture) {
                let interaction = builder.complete_with_error(e.to_string());
                if let Err(capture_err) = capture.record(interaction).await {
                    warn!("Failed to record debug capture: {}", capture_err);
                }
            }
            return Err(TryRequestError::Other(e.into()));
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        let captured_headers = debug_capture
            .as_ref()
            .filter(|capture| capture.headers_enabled())
            .map(|_| sanitized_capture_headers(resp.headers()));
        let body = match resp.text().await {
            Ok(body) => body,
            Err(error) => {
                persist_debug_capture(
                    debug_capture.as_ref(),
                    capture_builder,
                    status.as_u16(),
                    "",
                    captured_headers,
                    Some(error.to_string()),
                )
                .await;
                return Err(TryRequestError::Other(error.into()));
            }
        };
        let provider_error = format!("Provider returned {status} from {url}");
        persist_debug_capture(
            debug_capture.as_ref(),
            capture_builder,
            status.as_u16(),
            &body,
            captured_headers,
            Some(provider_error),
        )
        .await;

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            record_rate_limit_hit(tier_name);
            ratelimit_tracker.record_429(tier_name, retry_after);
            record_rate_limit_backoff(tier_name);

            return Err(TryRequestError::RateLimited(retry_after));
        }

        return Err(TryRequestError::Other(anyhow::anyhow!(
            "Provider returned {} from {}: {}",
            status,
            url,
            body
        )));
    }

    // Handle streaming vs non-streaming.
    if stream_flag {
        // For streaming, we need to translate OpenAI SSE events to Anthropic SSE.
        let rate_limit_info = extract_rate_limit_headers(&resp);
        let resp_status = resp.status().as_u16();
        let captured_headers = debug_capture
            .as_ref()
            .filter(|capture| capture.headers_enabled())
            .map(|_| sanitized_capture_headers(resp.headers()));

        // Peek at first chunk to detect errors embedded in 200 streams.
        let byte_stream = match check_stream_for_embedded_error(
            resp,
            tier_name,
            stream_first_event_timeout,
        )
        .await
        {
            Ok(stream) => stream,
            Err(error) => {
                persist_debug_capture(
                    debug_capture.as_ref(),
                    capture_builder,
                    resp_status,
                    "",
                    captured_headers,
                    Some(error.to_string()),
                )
                .await;
                return Err(error);
            }
        };

        let ctx = StreamVerifyCtx {
            tier_name: tier_name.to_string(),
            local_estimate,
            ratelimit_tracker: Some(ratelimit_tracker.clone()),
            rate_limit_info: Some(rate_limit_info),
            stream_start: std::time::Instant::now(),
            stream_idle_timeout,
        };
        Ok(stream_response_translated(
            byte_stream,
            config.sse_buffer_size(),
            Some(ctx),
            model_name,
            chain,
        )
        .await)
    } else {
        // Extract rate limit headers for non-streaming.
        let rate_limit_info = extract_rate_limit_headers(&resp);

        // For non-streaming, translate the full response.
        let resp_status = resp.status().as_u16();
        let captured_headers = debug_capture
            .as_ref()
            .filter(|capture| capture.headers_enabled())
            .map(|_| sanitized_capture_headers(resp.headers()));
        let body = resp
            .bytes()
            .await
            .map_err(|e| TryRequestError::Other(e.into()))?;

        // Check for embedded error in 200 body BEFORE recording success,
        // otherwise a failed request corrupts tier rate-limit state.
        if let Err(error) = check_body_for_embedded_error(&body, tier_name) {
            let body_str = String::from_utf8_lossy(&body);
            persist_debug_capture(
                debug_capture.as_ref(),
                capture_builder,
                resp_status,
                &body_str,
                captured_headers,
                Some(error.to_string()),
            )
            .await;
            return Err(error);
        }

        ratelimit_tracker.record_success(tier_name, rate_limit_info.0, rate_limit_info.1);

        let body_str = String::from_utf8_lossy(&body);

        // Record capture for non-streaming response
        if let (Some(builder), Some(capture)) = (capture_builder, debug_capture.clone()) {
            let interaction = builder.complete(resp_status, &body_str, captured_headers, None);
            if let Err(capture_err) = capture.record(interaction).await {
                warn!("Failed to record debug capture: {}", capture_err);
            }
        }

        // Try to parse as OpenAI response and translate.
        if let Ok(openai_resp) = serde_json::from_slice::<OpenAIResponse>(&body) {
            // Record usage from the response.
            if let Some(ref usage) = openai_resp.usage {
                record_usage(
                    tier_name,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    0, // OpenAI doesn't have cache fields in the same way
                    0,
                );
                verify_token_usage(tier_name, local_estimate, usage.prompt_tokens);
            }

            // Translate to Anthropic format.
            let anthropic_resp = translate_response_openai_to_anthropic(openai_resp, model_name);

            // Apply response transformers if chain is not empty.
            let final_resp = if chain.is_empty() {
                anthropic_resp
            } else {
                let resp_value = serde_json::to_value(&anthropic_resp)
                    .map_err(|e| TryRequestError::Other(e.into()))?;
                let transformed = chain
                    .apply_response(resp_value)
                    .map_err(TryRequestError::Other)?;
                serde_json::from_value::<AnthropicResponse>(transformed).unwrap_or(anthropic_resp)
            };

            let response_body =
                serde_json::to_vec(&final_resp).map_err(|e| TryRequestError::Other(e.into()))?;

            let mut response = (StatusCode::OK, response_body).into_response();
            insert_ccr_tier_header(&mut response, tier_name);
            return Ok(response);
        }

        // Fallback: pass through original response if translation fails.
        let mut response = (StatusCode::OK, body).into_response();
        insert_ccr_tier_header(&mut response, tier_name);
        Ok(response)
    }
}

pub(super) async fn try_request_via_anthropic_protocol(
    config: &Config,
    provider: &crate::config::Provider,
    args: TryRequestProtocolArgs<'_>,
) -> Result<Response, TryRequestError> {
    let TryRequestProtocolArgs {
        transformed_request,
        model_name,
        tier_name,
        local_estimate,
        stream_first_event_timeout,
        stream_idle_timeout,
        ratelimit_tracker,
        chain,
        debug_capture,
        openai_passthrough_body: _, // not used for Anthropic protocol
    } = args;

    let url = provider_anthropic_messages_url(provider);
    let headers = build_anthropic_headers(provider)?;

    trace!(tier = tier_name, model = model_name, url = %url, "dispatching Anthropic-compatible upstream request");

    let request: AnthropicRequest = serde_json::from_value(transformed_request.clone())
        .map_err(|e| TryRequestError::Other(e.into()))?;

    let needs_normalization = request
        .messages
        .iter()
        .any(|message| message.role == "tool");

    // Only canonicalize when OpenAI-style tool result messages are present.
    // Native Anthropic payloads should skip this round-trip to preserve
    // provider-specific content blocks (e.g., cache_control, thinking blocks).
    let mut normalized_request_value = if needs_normalization {
        let openai_request = translate_request_anthropic_to_openai(&request, model_name);
        let openai_request_value =
            serde_json::to_value(openai_request).map_err(|e| TryRequestError::Other(e.into()))?;
        OpenAiToAnthropicTransformer
            .transform_request(openai_request_value)
            .map_err(TryRequestError::Other)?
    } else {
        transformed_request
    };

    if let Some(obj) = normalized_request_value.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(model_name.to_string()),
        );
    }

    let request: AnthropicRequest = serde_json::from_value(normalized_request_value.clone())
        .map_err(|e| TryRequestError::Other(e.into()))?;

    // Set up capture if enabled for this provider
    let capture_builder = if let Some(ref capture) = debug_capture {
        if capture.should_capture(&provider.name) {
            let mut builder = capture
                .builder(&provider.name, tier_name)
                .model(model_name)
                .url(&url)
                .request_body(normalized_request_value)
                .streaming(request.stream.unwrap_or(false));
            if capture.headers_enabled() {
                builder = builder.request_headers(sanitized_capture_headers(&headers));
            }
            Some(builder.start())
        } else {
            None
        }
    } else {
        None
    };

    let resp = config
        .http_client()
        .post(&url)
        .headers(headers)
        .json(&request)
        .send()
        .await;

    // Handle connection errors with capture
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            // Record capture on connection failure
            if let (Some(builder), Some(capture)) = (capture_builder, debug_capture) {
                let interaction = builder.complete_with_error(e.to_string());
                if let Err(capture_err) = capture.record(interaction).await {
                    warn!("Failed to record debug capture: {}", capture_err);
                }
            }
            return Err(TryRequestError::Other(e.into()));
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        let captured_headers = debug_capture
            .as_ref()
            .filter(|capture| capture.headers_enabled())
            .map(|_| sanitized_capture_headers(resp.headers()));
        let body = match resp.text().await {
            Ok(body) => body,
            Err(error) => {
                persist_debug_capture(
                    debug_capture.as_ref(),
                    capture_builder,
                    status.as_u16(),
                    "",
                    captured_headers,
                    Some(error.to_string()),
                )
                .await;
                return Err(TryRequestError::Other(error.into()));
            }
        };
        let provider_error = format!("Provider returned {status} from {url}");
        persist_debug_capture(
            debug_capture.as_ref(),
            capture_builder,
            status.as_u16(),
            &body,
            captured_headers,
            Some(provider_error),
        )
        .await;

        // For 429 rate limit, pass through to let coordinator/client handle routing
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            record_rate_limit_hit(tier_name);
            ratelimit_tracker.record_429(tier_name, retry_after);
            record_rate_limit_backoff(tier_name);

            return Err(TryRequestError::RateLimited(retry_after));
        }

        return Err(TryRequestError::Other(anyhow::anyhow!(
            "Provider returned {} from {}: {}",
            status,
            url,
            body
        )));
    }

    if request.stream.unwrap_or(false) {
        // Use streaming token tracking for Anthropic protocol
        let rate_limit_info = extract_rate_limit_headers(&resp);
        let resp_status = resp.status().as_u16();
        let captured_headers = debug_capture
            .as_ref()
            .filter(|capture| capture.headers_enabled())
            .map(|_| sanitized_capture_headers(resp.headers()));

        // Peek at first chunk to detect errors embedded in 200 streams.
        let byte_stream = match check_stream_for_embedded_error(
            resp,
            tier_name,
            stream_first_event_timeout,
        )
        .await
        {
            Ok(stream) => stream,
            Err(error) => {
                persist_debug_capture(
                    debug_capture.as_ref(),
                    capture_builder,
                    resp_status,
                    "",
                    captured_headers,
                    Some(error.to_string()),
                )
                .await;
                return Err(error);
            }
        };

        let ctx = StreamVerifyCtx {
            tier_name: tier_name.to_string(),
            local_estimate,
            ratelimit_tracker: Some(ratelimit_tracker.clone()),
            rate_limit_info: Some(rate_limit_info),
            stream_start: std::time::Instant::now(),
            stream_idle_timeout,
        };

        let mut response = stream_anthropic_response_with_tracking(
            byte_stream,
            config.sse_buffer_size(),
            ctx,
            chain,
        )
        .await;
        insert_ccr_tier_header(&mut response, tier_name);
        Ok(response)
    } else {
        let rate_limit_info = extract_rate_limit_headers(&resp);

        let resp_status = resp.status().as_u16();
        let status = reqwest_status_to_axum(resp.status());
        let captured_headers = debug_capture
            .as_ref()
            .filter(|capture| capture.headers_enabled())
            .map(|_| sanitized_capture_headers(resp.headers()));
        let body = resp
            .bytes()
            .await
            .map_err(|e| TryRequestError::Other(e.into()))?;

        // Check for embedded error before recording success.
        if let Err(error) = check_body_for_embedded_error(&body, tier_name) {
            let body_str = String::from_utf8_lossy(&body);
            persist_debug_capture(
                debug_capture.as_ref(),
                capture_builder,
                resp_status,
                &body_str,
                captured_headers,
                Some(error.to_string()),
            )
            .await;
            return Err(error);
        }

        ratelimit_tracker.record_success(tier_name, rate_limit_info.0, rate_limit_info.1);

        let body_str = String::from_utf8_lossy(&body);

        // Record capture for non-streaming Anthropic response
        if let (Some(builder), Some(capture)) = (capture_builder, debug_capture.clone()) {
            let interaction = builder.complete(resp_status, &body_str, captured_headers, None);
            if let Err(capture_err) = capture.record(interaction).await {
                warn!("Failed to record debug capture: {}", capture_err);
            }
        }

        let transformed_response_value = match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(response_value) if !chain.is_empty() => Some(
                chain
                    .apply_response(response_value)
                    .map_err(TryRequestError::Other)?,
            ),
            Ok(response_value) => Some(response_value),
            Err(_) => None,
        };

        if let Some(response_value) = transformed_response_value {
            let anthropic_resp = match serde_json::from_value::<AnthropicResponse>(response_value) {
                Ok(response) => response,
                Err(_) => {
                    let mut response = (status, body).into_response();
                    insert_ccr_tier_header(&mut response, tier_name);
                    return Ok(response);
                }
            };
            // Estimate tokens when provider returns zeros (common for some Anthropic-compatible APIs)
            let mut input_tokens = anthropic_resp.usage.input_tokens;
            let mut output_tokens = anthropic_resp.usage.output_tokens;

            // Use pre-request estimate for input if provider returned 0
            if input_tokens == 0 && local_estimate > 0 {
                input_tokens = local_estimate;
                warn!(
                    tier = tier_name,
                    "Provider returned 0 input_tokens, using pre-request estimate: {}",
                    input_tokens
                );
            }

            // Estimate output tokens from response content if provider returned 0
            if output_tokens == 0 {
                let content_len: usize = anthropic_resp
                    .content
                    .iter()
                    .map(|block| match block {
                        AnthropicContentBlock::Text { text } => text.len(),
                        AnthropicContentBlock::Thinking { thinking, .. } => thinking.len(),
                        AnthropicContentBlock::ToolUse { input, .. } => input.to_string().len(),
                    })
                    .sum();
                if content_len > 0 {
                    output_tokens = (content_len / 4).max(1) as u64;
                    warn!(
                        tier = tier_name,
                        "Provider returned 0 output_tokens, estimated: {} (from {} chars)",
                        output_tokens,
                        content_len
                    );
                }
            }

            record_usage(tier_name, input_tokens, output_tokens, 0, 0);
            verify_token_usage(tier_name, local_estimate, input_tokens);

            let response_body = serde_json::to_vec(&anthropic_resp)
                .map_err(|e| TryRequestError::Other(e.into()))?;
            let mut response = (StatusCode::OK, response_body).into_response();
            insert_ccr_tier_header(&mut response, tier_name);
            return Ok(response);
        }

        let mut response = (status, body).into_response();
        insert_ccr_tier_header(&mut response, tier_name);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Provider;

    fn anthropic_provider(auth_header: Option<&str>) -> Provider {
        let mut value = serde_json::json!({
            "name": "test-anthropic",
            "api_base_url": "https://api.example.test/anthropic/v1",
            "api_key": "ak-test",
            "models": ["model-v1"],
            "protocol": "anthropic"
        });
        if let Some(header) = auth_header {
            value["auth_header"] = serde_json::Value::String(header.to_string());
        }
        serde_json::from_value(value).expect("provider config should parse")
    }

    #[test]
    fn anthropic_headers_default_to_x_api_key() {
        let headers = build_anthropic_headers(&anthropic_provider(None)).unwrap();

        assert_eq!(headers.get("x-api-key").unwrap(), "ak-test");
        assert!(headers.get("authorization").is_none());
    }

    #[test]
    fn anthropic_headers_can_use_bearer_authorization() {
        let headers = build_anthropic_headers(&anthropic_provider(Some("authorization"))).unwrap();

        assert_eq!(headers.get("authorization").unwrap(), "Bearer ak-test");
        assert!(headers.get("x-api-key").is_none());
    }

    #[test]
    fn debug_capture_headers_are_bounded_and_redacted() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_static("Bearer secret"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-api-key"),
            reqwest::header::HeaderValue::from_static("also-secret"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-request-id"),
            reqwest::header::HeaderValue::from_static("request-123"),
        );

        let captured = sanitized_capture_headers(&headers);

        assert_eq!(captured["authorization"], "[REDACTED]");
        assert_eq!(captured["x-api-key"], "[REDACTED]");
        assert_eq!(captured["x-request-id"], "request-123");
    }
}
