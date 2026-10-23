use super::*;

#[test]
fn provider_endpoint_suffix_requires_a_path_boundary() {
    let provider: crate::config::Provider = serde_json::from_value(serde_json::json!({
        "name": "suffix",
        "api_base_url": "https://example.test/fooresponses",
        "api_key": "test",
        "protocol": "responses",
        "models": []
    }))
    .unwrap();

    assert_eq!(
        provider_endpoint_url(&provider, "responses"),
        "https://example.test/fooresponses/responses"
    );
}
