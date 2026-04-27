// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use tracing::debug;

/// Normalized usage metrics containing token counts.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// Extract normalized usage metrics from various response formats.
///
/// Supports:
/// - OpenAI-style responses with `prompt_tokens_details.cached_tokens`
/// - Minimax-style responses with `cache_creation_input_tokens` and `cache_read_input_tokens`
/// - Anthropic-style responses with direct `input_tokens` and `output_tokens`
pub fn extract_normalized_usage(response: &Value) -> NormalizedUsage {
    let usage = match response.get("usage") {
        Some(u) => u,
        None => {
            debug!("no usage field in response");
            return default_normalized_usage();
        }
    };

    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // OpenAI-style: cached tokens are reported in prompt_tokens_details.cached_tokens
    let cached_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // Minimax-style: cache creation and read tokens are reported separately
    let cache_creation_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // If we have cached tokens from OpenAI-style, add them to cache_read_tokens
    let total_cache_read = cache_read_tokens.saturating_add(cached_tokens);

    NormalizedUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens: total_cache_read,
        cache_creation_tokens,
    }
}

/// Default normalized usage with all zeros.
fn default_normalized_usage() -> NormalizedUsage {
    NormalizedUsage {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_metrics_extracts_openai_cached_tokens() {
        // OpenAI-style response with cached tokens in prompt_tokens_details
        let response = json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "prompt_tokens_details": {
                    "cached_tokens": 30
                }
            }
        });

        let normalized = extract_normalized_usage(&response);
        assert_eq!(normalized.input_tokens, 100);
        assert_eq!(normalized.output_tokens, 50);
        assert_eq!(normalized.cache_read_tokens, 30); // cached tokens added to cache_read
        assert_eq!(normalized.cache_creation_tokens, 0);
    }

    #[test]
    fn usage_metrics_extracts_minimax_cache_fields() {
        // Minimax-style response with separate cache creation and read tokens
        let response = json!({
            "usage": {
                "input_tokens": 1,
                "output_tokens": 242,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 40161
            }
        });

        let normalized = extract_normalized_usage(&response);
        assert_eq!(normalized.input_tokens, 1);
        assert_eq!(normalized.output_tokens, 242);
        assert_eq!(normalized.cache_read_tokens, 40161);
        assert_eq!(normalized.cache_creation_tokens, 0);
    }

    #[test]
    fn usage_metrics_handles_anthropic_style() {
        // Anthropic-style response with direct input/output tokens
        let response = json!({
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500
            }
        });

        let normalized = extract_normalized_usage(&response);
        assert_eq!(normalized.input_tokens, 1000);
        assert_eq!(normalized.output_tokens, 500);
        assert_eq!(normalized.cache_read_tokens, 0);
        assert_eq!(normalized.cache_creation_tokens, 0);
    }

    #[test]
    fn usage_metrics_defaults_when_no_usage() {
        let response = json!({});
        let normalized = extract_normalized_usage(&response);
        assert_eq!(normalized.input_tokens, 0);
        assert_eq!(normalized.output_tokens, 0);
        assert_eq!(normalized.cache_read_tokens, 0);
        assert_eq!(normalized.cache_creation_tokens, 0);
    }

    #[test]
    fn usage_metrics_combines_openai_cache_with_minimax() {
        // Response with both OpenAI cached tokens and Minimax cache fields
        let response = json!({
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "prompt_tokens_details": {
                    "cached_tokens": 3
                },
                "cache_creation_input_tokens": 2,
                "cache_read_input_tokens": 7
            }
        });

        let normalized = extract_normalized_usage(&response);
        assert_eq!(normalized.input_tokens, 10);
        assert_eq!(normalized.output_tokens, 5);
        // cached_tokens (3) added to cache_read_tokens (7) = 10
        assert_eq!(normalized.cache_read_tokens, 10);
        assert_eq!(normalized.cache_creation_tokens, 2);
    }
}
