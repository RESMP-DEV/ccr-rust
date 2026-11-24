//! Tests for the provider-level model alias map.
//!
//! The map may only point at a model the provider actually serves, and it may
//! not alias a model onto itself. Both are configuration errors that must
//! surface when the configuration loads.

use super::*;

fn provider_with_map(model_map: &str) -> Provider {
    serde_json::from_str(&format!(
        r#"{{
            "name": "zai",
            "api_base_url": "https://api.z.ai/api/coding/paas/v4",
            "api_key": "test",
            "models": ["glm-5.4"],
            "modelMap": {model_map}
        }}"#
    ))
    .expect("provider fixture should parse")
}

/// A provider-level model map may only point at a model the provider actually
/// serves, so an alias to an unregistered id is a configuration error instead
/// of a silent request for a model nobody serves.
#[test]
fn provider_model_map_target_must_be_listed_in_models() {
    let provider = provider_with_map(r#"{"glm-5.3": "glm-5.9"}"#);

    let error = validate_provider_contracts(&[provider]).unwrap_err();

    assert!(
        error.to_string().contains(
            "modelMap target 'glm-5.9' for requested model 'glm-5.3' is not listed in the provider models list"
        ),
        "unexpected error: {error}"
    );
}

/// Aliasing a model onto itself is a no-op that hides a mistake in the map, so
/// it is rejected instead of silently accepted.
#[test]
fn provider_model_map_must_not_map_a_model_onto_itself() {
    let provider = provider_with_map(r#"{"glm-5.4": "glm-5.4"}"#);

    let error = validate_provider_contracts(&[provider]).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("modelMap entry 'glm-5.4' must not map a model onto itself"),
        "unexpected error: {error}"
    );
}

/// A well-formed map is accepted: the alias points at a registered model and
/// at a different one than the requested id.
#[test]
fn provider_model_map_with_registered_target_is_accepted() {
    let provider = provider_with_map(r#"{"glm-5.3": "glm-5.4"}"#);

    assert_eq!(
        provider
            .model_map
            .get("glm-5.3")
            .map(|mapped| mapped.as_str()),
        Some("glm-5.4")
    );
    validate_provider_contracts(&[provider]).unwrap();
}
