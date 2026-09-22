// SPDX-License-Identifier: AGPL-3.0-or-later
//! Preserve native controls and translate explicit OpenAI effort without
//! guessing thinking modes or numerical budgets. Providers validate values.

use serde_json::{Map, Value};

pub(super) fn from_extra_params(extra: Option<&Value>) -> (Option<Value>, Option<Value>) {
    let Some(extra) = extra else {
        return (None, None);
    };
    let thinking = extra
        .get("thinking")
        .filter(|value| !value.is_null())
        .cloned();
    let mut output_config = extra
        .get("output_config")
        .filter(|value| !value.is_null())
        .cloned();
    if let Some(effort) = extra.get("reasoning_effort").and_then(Value::as_str) {
        let output = output_config.get_or_insert_with(|| Value::Object(Map::new()));
        // Keep malformed native values intact for upstream validation as well.
        // An explicit native effort takes precedence over the translated field.
        if let Some(object) = output.as_object_mut() {
            object
                .entry("effort")
                .or_insert_with(|| Value::String(effort.to_owned()));
        }
    }
    (thinking, output_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn effort_labels_are_preserved_without_inventing_a_budget() {
        for effort in [
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "future",
        ] {
            let (thinking, output) = from_extra_params(Some(&json!({"reasoning_effort": effort})));
            assert_eq!(thinking, None);
            assert_eq!(output, Some(json!({"effort": effort})));
        }
    }

    #[test]
    fn native_controls_win_and_keep_provider_extensions() {
        let thinking = json!({"type": "enabled", "budget_tokens": 4096, "display": "omitted"});
        let output = json!({"effort": "high", "format": {"type": "json_schema", "schema": {"type": "object"}}});
        assert_eq!(
            from_extra_params(Some(&json!({
                "thinking": thinking, "output_config": output, "reasoning_effort": "max",
                "unrelated": "must not be forwarded"
            }))),
            (Some(thinking), Some(output))
        );
    }

    #[test]
    fn absent_or_null_controls_remain_absent() {
        assert_eq!(from_extra_params(None), (None, None));
        assert_eq!(
            from_extra_params(Some(
                &json!({"thinking": null, "output_config": null, "reasoning_effort": null})
            )),
            (None, None)
        );
    }

    #[test]
    fn native_effort_overrides_openai_model_defaults() {
        let request = serde_json::from_value(json!({
            "model": "test", "messages": [], "output_config": {"effort": "max"}
        }))
        .unwrap();
        for model in ["test-model", "deepseek-reasoner"] {
            let translated = super::super::translate_request::translate_request_anthropic_to_openai(
                &request, model,
            );
            assert_eq!(translated.reasoning_effort.as_deref(), Some("max"));
        }
    }
}
