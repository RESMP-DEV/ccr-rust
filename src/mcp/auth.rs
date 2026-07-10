// SPDX-License-Identifier: AGPL-3.0-or-later
use anyhow::{bail, Result};
use axum::http::{header::AUTHORIZATION, HeaderMap};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub(super) struct BearerAuth {
    token_digest: [u8; 32],
}

impl BearerAuth {
    pub(super) fn new(token: String) -> Result<Self> {
        if token.is_empty()
            || !token.is_ascii()
            || token
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            bail!("MCP daemon bearer token must be nonempty ASCII without whitespace");
        }

        Ok(Self {
            token_digest: Sha256::digest(token.as_bytes()).into(),
        })
    }

    pub(super) fn authorizes(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers.get(AUTHORIZATION) else {
            return false;
        };
        let bytes = value.as_bytes();
        let Some(separator) = bytes.iter().position(|byte| *byte == b' ') else {
            return false;
        };
        let (scheme, token) = bytes.split_at(separator);
        let token = &token[1..];
        if !scheme.eq_ignore_ascii_case(b"Bearer") || token.is_empty() {
            return false;
        }

        let candidate_digest: [u8; 32] = Sha256::digest(token).into();
        self.token_digest.ct_eq(&candidate_digest).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn bearer_auth_rejects_missing_and_wrong_tokens() {
        let auth = BearerAuth::new("correct-token".to_string()).unwrap();
        assert!(!auth.authorizes(&HeaderMap::new()));

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token"),
        );
        assert!(!auth.authorizes(&headers));
    }

    #[test]
    fn bearer_auth_accepts_correct_token_and_case_insensitive_scheme() {
        let auth = BearerAuth::new("correct-token".to_string()).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("bearer correct-token"),
        );
        assert!(auth.authorizes(&headers));
    }

    #[test]
    fn bearer_auth_rejects_unsafe_configured_tokens() {
        assert!(BearerAuth::new(String::new()).is_err());
        assert!(BearerAuth::new("has whitespace".to_string()).is_err());
        assert!(BearerAuth::new("non-ascii-é".to_string()).is_err());
    }
}
