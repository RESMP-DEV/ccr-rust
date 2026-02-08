//! Frontend detection helpers for Codex and Claude Code clients.

use axum::http::{header::USER_AGENT, HeaderMap};
use serde_json::Value;

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
/// Rules:
/// - Claude Code: any `anthropic-*` header OR body has Anthropic format (top-level `model` + `messages` without `role`)
/// - Codex: `User-Agent` contains `codex` OR body has OpenAI format (`messages` with `role`)
/// - Default: Codex (primary execution engine).
pub fn detect_frontend(headers: &HeaderMap, body: &Value) -> FrontendType {
    let claude_signal = has_anthropic_headers(headers) || has_anthropic_format(body);
    let codex_signal = has_codex_user_agent(headers) || has_openai_format(body);

    if claude_signal && !codex_signal {
        FrontendType::ClaudeCode
    } else {
        FrontendType::Codex
    }
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
            "model": "claude-3-5-sonnet",
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
}
