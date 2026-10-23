use super::TryRequestError;
use crate::config::{Provider, ProviderProtocol};
use tracing::trace;

/// Apply validated provider-level request requirements immediately before dispatch.
pub(super) fn apply_provider_request_overrides(
    provider: &Provider,
    request: &mut serde_json::Value,
) -> Result<(), TryRequestError> {
    if provider.force_adaptive_thinking {
        let object = request.as_object_mut().ok_or_else(|| {
            TryRequestError::Other(anyhow::anyhow!(
                "provider '{}' request override requires a JSON object",
                provider.name
            ))
        })?;
        object.insert(
            "thinking".to_string(),
            serde_json::json!({"type": "adaptive"}),
        );
    }
    let Some(reasoning_effort) = provider.force_reasoning_effort.as_deref() else {
        return Ok(());
    };
    if provider.protocol != ProviderProtocol::Openai {
        return Err(TryRequestError::Other(anyhow::anyhow!(
            "provider '{}' force_reasoning_effort requires protocol 'openai'",
            provider.name
        )));
    }
    let object = request.as_object_mut().ok_or_else(|| {
        TryRequestError::Other(anyhow::anyhow!(
            "provider '{}' request override requires a JSON object",
            provider.name
        ))
    })?;
    object.insert(
        "reasoning_effort".to_string(),
        serde_json::Value::String(reasoning_effort.to_string()),
    );
    trace!(
        provider = %provider.name,
        reasoning_effort,
        "forced provider reasoning effort"
    );
    Ok(())
}
