//! Provider-level model alias map.
//!
//! A provider can declare that a requested model id is served by a different
//! upstream model id. The map is the operator's explicit statement of which
//! model answers a request, so it wins over the router's built-in model
//! canonicalization rules. It exists so a retired model name can be aliased
//! onto its successor without changing every client pin.

use anyhow::Result;

use super::types::Provider;

/// Resolve the upstream model id for a requested model id.
///
/// Returns `None` when the provider does not alias the requested name, in
/// which case the caller keeps the requested id and applies its own rules.
pub(crate) fn resolve<'p>(provider: &'p Provider, model_name: &str) -> Option<&'p str> {
    provider.model_map.get(model_name).map(String::as_str)
}

/// Validate a provider's model map at load time.
///
/// A map target must be a model the provider actually serves, and an entry
/// must not map a model onto itself. Both are configuration errors the
/// operator has to see when the configuration loads instead of a silent
/// no-op at dispatch time.
pub(crate) fn validate(provider: &Provider) -> Result<()> {
    for (requested, upstream) in &provider.model_map {
        anyhow::ensure!(
            provider.models.iter().any(|configured| configured == upstream),
            "provider '{}' modelMap target '{}' for requested model '{}' is not listed in the provider models list",
            provider.name,
            upstream,
            requested
        );
        anyhow::ensure!(
            requested != upstream,
            "provider '{}' modelMap entry '{}' must not map a model onto itself",
            provider.name,
            requested
        );
    }
    Ok(())
}
