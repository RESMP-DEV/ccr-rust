//! Tests for the provider-level model map applied at dispatch time.
//!
//! The map is the operator's explicit statement of which upstream model
//! serves a requested id, so it is consulted before the built-in model
//! canonicalization rules.

use super::*;
use crate::config::Provider;

/// A provider-level model map resolves the requested name to the upstream id
/// the provider serves, and leaves every other name alone.
#[test]
fn provider_model_map_resolves_requested_model() {
    let provider: Provider = serde_json::from_value(serde_json::json!({
        "name": "zai",
        "api_base_url": "https://api.z.ai/api/coding/paas/v4",
        "api_key": "test",
        "models": ["glm-5.4"],
        "modelMap": {"glm-5.3": "glm-5.4"}
    }))
    .unwrap();

    // A mapped name resolves to the upstream id the provider serves.
    assert_eq!(canonical_model_name(&provider, "glm-5.3"), "glm-5.4");
    // An unmapped name keeps the requested id.
    assert_eq!(canonical_model_name(&provider, "glm-5.4"), "glm-5.4");
    assert_eq!(
        canonical_model_name(&provider, "some-other-model"),
        "some-other-model"
    );
}

/// The provider model map is consulted before the MiniMax pinning, so an
/// operator can steer an M3-family route at a different upstream model.
#[test]
fn provider_model_map_takes_precedence_over_minimax_pinning() {
    let provider: Provider = serde_json::from_value(serde_json::json!({
        "name": "minimax",
        "api_base_url": "https://api.minimax.example/v1",
        "api_key": "test",
        "models": ["MiniMax-M3.1-Flash", "MiniMax-M2.7"],
        "modelMap": {"MiniMax-M3": "MiniMax-M2.7"}
    }))
    .unwrap();

    // Without the map this request would pin to MiniMax-M3.1-Flash.
    assert_eq!(
        canonical_model_name(&provider, "MiniMax-M3"),
        "MiniMax-M2.7"
    );
    // Names the map does not list keep the built-in pinning.
    assert_eq!(
        canonical_model_name(&provider, "MiniMax-M3.1-Flash-Preview"),
        "MiniMax-M3.1-Flash"
    );
}
