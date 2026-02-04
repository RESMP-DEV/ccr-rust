//! Max tokens transformer.
//!
//! Overrides the `max_tokens` field in requests to ensure consistent limits.
//! Supports:
//! - Setting a specific max_tokens value
//! - Conditionally overriding only if the new value is higher than existing

use crate::transformer::Transformer;
use anyhow::Result;
use serde_json::Value;
use tracing::debug;

/// Max tokens transformer configuration.
///
/// Options:
/// - `max_tokens: u32` (required) - The value to set
/// - `override_if_higher: bool` (default: true) - Only set if higher than existing
#[derive(Debug, Clone)]
pub struct MaxTokenTransformer {
    max_tokens: u32,
    override_if_higher: bool,
}

impl MaxTokenTransformer {
    /// Create a new max tokens transformer with the given limit.
    ///
    /// By default, only overrides if the configured limit is higher than
    /// the existing value. Set `override_if_higher` to false to always
    /// set the configured value.
    pub fn new(max_tokens: u32, override_if_higher: bool) -> Self {
        Self {
            max_tokens,
            override_if_higher,
        }
    }

    /// Create a transformer from JSON options.
    ///
    /// Expected options format:
    /// ```json
    /// {
    ///   "max_tokens": 65536,
    ///   "override_if_higher": true
    /// }
    /// ```
    pub fn from_options(options: &Value) -> Result<Self> {
        let max_tokens = options
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("max_tokens is required for maxtoken transformer"))?
            as u32;

        let override_if_higher = options
            .get("override_if_higher")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        Ok(Self::new(max_tokens, override_if_higher))
    }
}

impl Transformer for MaxTokenTransformer {
    fn name(&self) -> &str {
        "maxtoken"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        if let Some(request_obj) = request.as_object_mut() {
            if let Some(existing) = request_obj.get("max_tokens") {
                if let Some(current) = existing.as_u64() {
                    if self.override_if_higher {
                        // Only override if our limit is higher
                        if self.max_tokens as u64 > current {
                            debug!(
                                from = current,
                                to = self.max_tokens,
                                "overriding max_tokens (higher)"
                            );
                            request_obj.insert(
                                "max_tokens".to_string(),
                                Value::Number(serde_json::Number::from(self.max_tokens)),
                            );
                        } else {
                            debug!(
                                current,
                                configured = self.max_tokens,
                                "keeping existing max_tokens (lower or equal)"
                            );
                        }
                    } else {
                        // Always override
                        debug!(
                            from = current,
                            to = self.max_tokens,
                            "forcing max_tokens override"
                        );
                        request_obj.insert(
                            "max_tokens".to_string(),
                            Value::Number(serde_json::Number::from(self.max_tokens)),
                        );
                    }
                } else {
                    // Existing value is not a number, override it
                    request_obj.insert(
                        "max_tokens".to_string(),
                        Value::Number(serde_json::Number::from(self.max_tokens)),
                    );
                }
            } else {
                // No max_tokens field, add it
                debug!(value = self.max_tokens, "adding max_tokens to request");
                request_obj.insert(
                    "max_tokens".to_string(),
                    Value::Number(serde_json::Number::from(self.max_tokens)),
                );
            }
        }
        Ok(request)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request() -> Value {
        serde_json::json!({
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        })
    }

    #[test]
    fn transformer_name() {
        let transformer = MaxTokenTransformer::new(65536, true);
        assert_eq!(transformer.name(), "maxtoken");
    }

    #[test]
    fn overrides_when_higher() {
        let transformer = MaxTokenTransformer::new(65536, true);
        let request = test_request();

        let result = transformer.transform_request(request).unwrap();
        assert_eq!(result["max_tokens"], 65536);
    }

    #[test]
    fn keeps_when_lower() {
        let transformer = MaxTokenTransformer::new(500, true);
        let request = test_request();

        let result = transformer.transform_request(request).unwrap();
        // Should keep the existing higher value
        assert_eq!(result["max_tokens"], 1000);
    }

    #[test]
    fn keeps_when_equal() {
        let transformer = MaxTokenTransformer::new(1000, true);
        let request = test_request();

        let result = transformer.transform_request(request).unwrap();
        assert_eq!(result["max_tokens"], 1000);
    }

    #[test]
    fn always_override_with_false() {
        let transformer = MaxTokenTransformer::new(500, false);
        let request = test_request();

        let result = transformer.transform_request(request).unwrap();
        // Should override even though 500 < 1000
        assert_eq!(result["max_tokens"], 500);
    }

    #[test]
    fn adds_if_missing() {
        let transformer = MaxTokenTransformer::new(65536, true);
        let mut request = test_request();
        request.as_object_mut().unwrap().remove("max_tokens");

        let result = transformer.transform_request(request).unwrap();
        assert_eq!(result["max_tokens"], 65536);
    }

    #[test]
    fn from_options_basic() {
        let options = serde_json::json!({"max_tokens": 65536});
        let transformer = MaxTokenTransformer::from_options(&options).unwrap();
        assert_eq!(transformer.max_tokens, 65536);
        assert!(transformer.override_if_higher); // default is true
    }

    #[test]
    fn from_options_with_flag() {
        let options = serde_json::json!({
            "max_tokens": 65536,
            "override_if_higher": false
        });
        let transformer = MaxTokenTransformer::from_options(&options).unwrap();
        assert_eq!(transformer.max_tokens, 65536);
        assert!(!transformer.override_if_higher);
    }

    #[test]
    fn from_options_missing_max_tokens() {
        let options = serde_json::json!({});
        let result = MaxTokenTransformer::from_options(&options);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("max_tokens is required"));
    }
}
