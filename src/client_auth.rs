// SPDX-License-Identifier: AGPL-3.0-or-later
use anyhow::{bail, Result};
use axum::extract::{Request, State};
use axum::http::{
    header::{self, AUTHORIZATION},
    HeaderMap, StatusCode,
};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// A client-facing API key, retained only as a digest.
///
/// Both `Authorization: Bearer <key>` and Anthropic-style `x-api-key: <key>`
/// are accepted. Offering both forms lets one CCR endpoint serve standard
/// OpenAI clients and native Anthropic clients without coupling either client
/// to CCR's upstream provider credentials.
#[derive(Clone, Debug)]
pub struct ClientApiKey {
    token_digest: [u8; 32],
}

impl ClientApiKey {
    pub fn new(token: String) -> Result<Self> {
        if token.is_empty()
            || !token.is_ascii()
            || token
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            bail!("CLIENT_API_KEY must be nonempty ASCII without whitespace");
        }

        Ok(Self {
            token_digest: Sha256::digest(token.as_bytes()).into(),
        })
    }

    pub fn authorizes(&self, headers: &HeaderMap) -> bool {
        self.bearer_authorizes(headers) || self.api_key_authorizes(headers)
    }

    fn bearer_authorizes(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let Some((scheme, token)) = value.split_once(' ') else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("Bearer") || token.is_empty() {
            return false;
        }
        self.matches(token)
    }

    fn api_key_authorizes(&self, headers: &HeaderMap) -> bool {
        headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .filter(|token| !token.is_empty())
            .is_some_and(|token| self.matches(token))
    }

    fn matches(&self, token: &str) -> bool {
        let candidate_digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        self.token_digest.ct_eq(&candidate_digest).into()
    }
}

pub async fn require_client_auth(
    State(state): State<crate::router::AppState>,
    request: Request,
    next: Next,
) -> Response {
    // Health remains local-service liveness data. Every API, preset,
    // observability, and metrics route is authenticated when a key is set.
    if request.uri().path() == "/health" {
        return next.run(request).await;
    }

    let authorized = state
        .config
        .client_api_key()
        .is_none_or(|key| key.authorizes(request.headers()));
    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            axum::Json(json!({
                "error": {
                    "message": "missing or invalid client API key",
                    "type": "authentication_error"
                }
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn client_key_accepts_standard_bearer_and_anthropic_api_key_forms() {
        let auth = ClientApiKey::new("correct-token".to_string()).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("bearer correct-token"),
        );
        assert!(auth.authorizes(&headers));

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("correct-token"));
        assert!(auth.authorizes(&headers));
    }

    #[test]
    fn client_key_rejects_missing_and_wrong_tokens() {
        let auth = ClientApiKey::new("correct-token".to_string()).unwrap();
        assert!(!auth.authorizes(&HeaderMap::new()));

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
        assert!(!auth.authorizes(&headers));

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("wrong"));
        assert!(!auth.authorizes(&headers));
    }

    #[test]
    fn client_key_rejects_unsafe_configured_tokens() {
        assert!(ClientApiKey::new(String::new()).is_err());
        assert!(ClientApiKey::new("has whitespace".to_string()).is_err());
        assert!(ClientApiKey::new("non-ascii-é".to_string()).is_err());
    }
}
