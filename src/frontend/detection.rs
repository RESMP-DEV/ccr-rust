// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frontend detection helpers for Codex and Claude Code clients.

use axum::http::{header::USER_AGENT, HeaderMap};
use serde_json::Value;
use tracing::debug;

/// Frontend type inferred from headers and request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontendType {
    /// OpenAI-compatible Codex frontend.
    Codex,
    /// Anthropic Claude Code frontend.
    ClaudeCode,
}

/// Detect the request frontend using lightweight format heuristics.
///
/// Rules (in priority order):
/// 1. Definitive header signals win: `anthropic-*` → ClaudeCode, `codex` User-Agent → Codex
/// 2. When both header signals conflict → Codex (explicit Codex UA overrides anthropic headers)
/// 3. Body-only heuristics: Anthropic format (model+messages) without OpenAI role fields → ClaudeCode
/// 4. Default: Codex
///
/// Note: The Anthropic Messages API includes `role` on every message, which overlaps
/// with OpenAI's format. Header-based detection takes priority to resolve this ambiguity.
pub fn detect_frontend(headers: &HeaderMap, body: &Value) -> FrontendType {
    let has_anthropic_hdrs = has_anthropic_headers(headers);
    let has_codex_ua = has_codex_user_agent(headers);

    // Header signals are definitive — resolve conflicts at the header level first.
    if has_anthropic_hdrs && !has_codex_ua {
        return FrontendType::ClaudeCode;
    }
    if has_codex_ua {
        return FrontendType::Codex;
    }

    // No header signals — fall back to body format heuristics.
    let claude_body = has_anthropic_format(body);
    let codex_body = has_openai_format(body);

    let result = if claude_body && !codex_body {
        FrontendType::ClaudeCode
    } else {
        FrontendType::Codex
    };
    debug!(?result, "frontend detected");
    result
}

fn has_codex_user_agent(headers: &HeaderMap) -> bool {
    headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|ua| ua.to_ascii_lowercase().contains("codex"))
}

fn has_anthropic_headers(headers: &HeaderMap) -> bool {
    headers
        .keys()
        .any(|name| name.as_str().to_ascii_lowercase().starts_with("anthropic-"))
}

fn has_openai_format(body: &Value) -> bool {
    body.get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            !messages.is_empty()
                && messages.iter().all(|message| {
                    message
                        .get("role")
                        .and_then(Value::as_str)
                        .is_some_and(|role| !role.is_empty())
                })
        })
}

fn has_anthropic_format(body: &Value) -> bool {
    body.get("model").and_then(Value::as_str).is_some()
        && body.get("messages").and_then(Value::as_array).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_codex_from_user_agent() {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, "codex-cli/1.0.0".parse().unwrap());

        assert_eq!(detect_frontend(&headers, &json!({})), FrontendType::Codex);
    }

    #[test]
    fn detects_codex_from_openai_messages_format() {
        let headers = HeaderMap::new();
        let body = json!({
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hi"}
            ]
        });

        assert_eq!(detect_frontend(&headers, &body), FrontendType::Codex);
    }

    #[test]
    fn detects_claude_code_from_anthropic_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());

        assert_eq!(
            detect_frontend(&headers, &json!({"messages": []})),
            FrontendType::ClaudeCode
        );
    }

    #[test]
    fn detects_claude_code_from_anthropic_body_shape() {
        let headers = HeaderMap::new();
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [{"content": "Hello"}]
        });

        assert_eq!(detect_frontend(&headers, &body), FrontendType::ClaudeCode);
    }

    #[test]
    fn defaults_to_codex_when_ambiguous_body() {
        let headers = HeaderMap::new();
        let body = json!({
            "model": "gpt-4.1",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        assert_eq!(detect_frontend(&headers, &body), FrontendType::Codex);
    }

    #[test]
    fn defaults_to_codex_when_ambiguous_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, "codex/0.1".parse().unwrap());
        headers.insert("anthropic-client-id", "abc".parse().unwrap());

        assert_eq!(detect_frontend(&headers, &json!({})), FrontendType::Codex);
    }

    #[test]
    fn defaults_to_codex_when_no_signal() {
        let headers = HeaderMap::new();

        assert_eq!(
            detect_frontend(&headers, &json!({"other": true})),
            FrontendType::Codex
        );
    }

    #[test]
    fn detects_claude_code_when_anthropic_headers_with_role_in_body() {
        // Real-world case: Claude Code CLI sends anthropic-version header
        // AND the Anthropic Messages API includes "role" on every message,
        // which overlaps with OpenAI format. Headers must take priority.
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        let body = json!({
            "model": "zai,glm-5.1",
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "Hello"}]
        });

        assert_eq!(detect_frontend(&headers, &body), FrontendType::ClaudeCode);
    }
}
