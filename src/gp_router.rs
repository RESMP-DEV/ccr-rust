// SPDX-License-Identifier: AGPL-3.0-or-later
use chrono::Timelike;
use gp_routing::features::BACKEND_SLOTS;
use gp_routing::{
    thompson_sample, ucb_score, FeatureVector, GpRoutingConfig, GpSurrogate, ObservationBuffer,
};
use parking_lot::Mutex;
use rand::Rng;
use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::config::{Config, GpAcquisitionStrategy, GpRoutingRuntimeConfig};
use crate::router::AnthropicRequest;

#[derive(Debug)]
pub struct GpRequestRouter {
    runtime_config: GpRoutingRuntimeConfig,
    model_config: GpRoutingConfig,
    surrogate: GpSurrogate,
    buffer: Mutex<ObservationBuffer>,
    backend_indices: HashMap<String, usize>,
}

#[derive(Clone, Debug)]
pub struct GpRoutingPlan {
    pub ordered: Vec<(String, String)>,
    request_features: RequestFeatureContext,
    tier_costs: HashMap<String, TierCostEstimate>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TierCostEstimate {
    usd: Option<f64>,
    relative: Option<f32>,
}

#[derive(Clone, Debug)]
struct RequestFeatureContext {
    prompt_chars: usize,
    estimated_input_tokens: u64,
    max_output_tokens: u64,
    request_class: u8,
    has_system_prompt: bool,
    message_count: usize,
    tool_count: usize,
    active_streams: usize,
    max_streams: usize,
    hour_of_day: u32,
}

impl RequestFeatureContext {
    fn from_request(request: &AnthropicRequest, active_streams: usize, max_streams: usize) -> Self {
        let prompt_chars = prompt_chars(request);
        Self {
            prompt_chars,
            estimated_input_tokens: estimate_input_tokens(prompt_chars),
            max_output_tokens: request.max_tokens.map_or(0, u64::from),
            request_class: classify_request(request),
            has_system_prompt: request.system.is_some(),
            message_count: request.messages.len(),
            tool_count: request.tools.as_ref().map_or(0, Vec::len),
            active_streams,
            max_streams,
            hour_of_day: chrono::Local::now().hour(),
        }
    }
}

#[derive(Clone, Debug)]
struct ScoredCandidate {
    canonical_index: usize,
    entry: (String, String),
    acquisition_score: f32,
    mean: f32,
    variance: f32,
    estimated_cost_usd: Option<f64>,
    credible: bool,
}

impl GpRequestRouter {
    pub fn new(runtime_config: GpRoutingRuntimeConfig, canonical_tiers: &[String]) -> Self {
        let backend_indices = canonical_tiers
            .iter()
            .take(BACKEND_SLOTS)
            .enumerate()
            .map(|(idx, tier)| (tier.clone(), idx))
            .collect::<HashMap<_, _>>();

        let model_config = GpRoutingConfig::builder()
            .buffer_capacity(runtime_config.buffer_capacity)
            .min_observations(runtime_config.min_observations)
            .refit_interval(runtime_config.refit_interval)
            .ucb_kappa(runtime_config.ucb_kappa)
            .nugget(runtime_config.nugget)
            .kpls_dim(runtime_config.kpls_dim)
            .prior_mean(runtime_config.prior_mean)
            .prior_variance(runtime_config.prior_variance)
            .n_backends(backend_indices.len().max(1))
            .build();

        if canonical_tiers.len() > BACKEND_SLOTS {
            warn!(
                configured_tiers = canonical_tiers.len(),
                gp_slots = BACKEND_SLOTS,
                "configured tiers exceed the bounded GP feature capacity; tail tiers remain in fallback order"
            );
        }

        Self {
            runtime_config,
            surrogate: GpSurrogate::new(model_config.clone()),
            buffer: Mutex::new(ObservationBuffer::new(model_config.buffer_capacity)),
            model_config,
            backend_indices,
        }
    }

    pub fn plan_rerank(
        &self,
        ordered: &[(String, String)],
        request: &AnthropicRequest,
        config: &Config,
        active_streams: usize,
        max_streams: usize,
        pinned_prefix_len: usize,
    ) -> GpRoutingPlan {
        let request_features =
            RequestFeatureContext::from_request(request, active_streams, max_streams);
        let tier_costs = estimate_tier_costs(ordered, &request_features, config);
        if !self.surrogate.is_fitted() {
            return GpRoutingPlan {
                ordered: ordered.to_vec(),
                request_features,
                tier_costs,
            };
        }

        let prefix_len = pinned_prefix_len.min(ordered.len());
        let candidate_positions = ordered
            .iter()
            .enumerate()
            .skip(prefix_len)
            .filter(|(_, (tier, _))| self.backend_indices.contains_key(tier))
            .map(|(idx, _)| idx)
            .take(self.runtime_config.max_candidates.min(BACKEND_SLOTS))
            .collect::<Vec<_>>();

        if candidate_positions.len() <= 1 {
            return GpRoutingPlan {
                ordered: ordered.to_vec(),
                request_features,
                tier_costs,
            };
        }

        let epsilon_pick = match self.runtime_config.acquisition {
            GpAcquisitionStrategy::EpsilonGreedy => {
                let mut rng = rand::thread_rng();
                if rng.gen::<f32>() < self.runtime_config.epsilon.clamp(0.0, 1.0) {
                    Some(rng.gen_range(0..candidate_positions.len()))
                } else {
                    None
                }
            }
            _ => None,
        };

        let mut scored = candidate_positions
            .iter()
            .enumerate()
            .filter_map(|(candidate_idx, pos)| {
                let entry = ordered.get(*pos)?.clone();
                let tier_cost = tier_costs.get(&entry.0).copied().unwrap_or_default();
                let features =
                    self.build_features(&request_features, &entry.0, 0, tier_cost.relative)?;
                let (mean, variance) = self.surrogate.predict(&features);
                let score = self.score_candidate(mean, variance, candidate_idx, epsilon_pick);
                Some(ScoredCandidate {
                    canonical_index: candidate_idx,
                    entry,
                    acquisition_score: score,
                    mean,
                    variance,
                    estimated_cost_usd: tier_cost.usd,
                    credible: false,
                })
            })
            .collect::<Vec<_>>();

        credible_set_cost_order(&mut scored, self.runtime_config.ucb_kappa);

        let mut reranked = ordered.to_vec();
        for (pos, candidate) in candidate_positions.iter().zip(scored.iter()) {
            reranked[*pos] = candidate.entry.clone();
        }

        if reranked != ordered {
            let scored_log = scored
                .iter()
                .map(|candidate| {
                    format!(
                        "{}(score={:.3}, mean={:.3}, var={:.3}, cost={:?}, credible={})",
                        candidate.entry.1,
                        candidate.acquisition_score,
                        candidate.mean,
                        candidate.variance,
                        candidate.estimated_cost_usd,
                        candidate.credible,
                    )
                })
                .collect::<Vec<_>>();
            debug!(
                prefix_len,
                scored = ?scored_log,
                before = ?ordered.iter().map(|(_, name)| name.clone()).collect::<Vec<_>>(),
                after = ?reranked.iter().map(|(_, name)| name.clone()).collect::<Vec<_>>(),
                "gp-routing reranked tiers"
            );
        }

        GpRoutingPlan {
            ordered: reranked,
            request_features,
            tier_costs,
        }
    }

    pub fn record_attempt(
        &self,
        plan: &GpRoutingPlan,
        tier: &str,
        attempt: usize,
        duration_secs: Option<f64>,
        _config: &Config,
    ) {
        let relative_cost = plan.tier_costs.get(tier).and_then(|cost| cost.relative);
        let Some(features) =
            self.build_features(&plan.request_features, tier, attempt, relative_cost)
        else {
            return;
        };

        let outcome = outcome_score(duration_secs);
        let mut buffer = self.buffer.lock();
        buffer.push(features, outcome);

        let should_fit = buffer.len() >= self.model_config.min_observations
            && (!self.surrogate.is_fitted()
                || (self.model_config.refit_interval > 0
                    && buffer.requests_since_last_fit() >= self.model_config.refit_interval));

        if should_fit {
            match self.surrogate.fit(&buffer) {
                Ok(()) => {
                    buffer.mark_fitted();
                    info!(
                        observations = buffer.len(),
                        fit_count = self.surrogate.fit_count(),
                        duration_ms = self.surrogate.last_fit_duration_ms(),
                        "gp-routing surrogate fit complete"
                    );
                }
                Err(err) => {
                    warn!(error = %err, "gp-routing surrogate fit failed");
                }
            }
        }
    }

    fn build_features(
        &self,
        request_features: &RequestFeatureContext,
        tier: &str,
        attempt: usize,
        relative_cost: Option<f32>,
    ) -> Option<FeatureVector> {
        let backend_index = *self.backend_indices.get(tier)?;
        let total_capacity = normalize_capacity(
            request_features.max_streams,
            request_features.active_streams,
        );
        let clamped_active = request_features.active_streams.min(total_capacity);
        let idle_capacity = total_capacity.saturating_sub(clamped_active);

        Some(
            FeatureVector::builder()
                .prompt_length(request_features.prompt_chars)
                // Reuse the categorical priority slots as request-shape buckets:
                // plain / streaming / tool-heavy / mixed.
                .priority(request_features.request_class)
                // Reuse the verify bit as a "has system prompt" indicator.
                .has_verify(request_features.has_system_prompt)
                .dependency_count(request_features.message_count.saturating_sub(1))
                .retry_count(attempt)
                .backend_index(backend_index, self.model_config.n_backends)
                .idle_ratio(idle_capacity, total_capacity)
                .in_flight_ratio(clamped_active, total_capacity)
                .hour_of_day(request_features.hour_of_day)
                .loop_count(request_features.tool_count)
                .relative_cost(relative_cost)
                .build(),
        )
    }

    fn score_candidate(
        &self,
        mean: f32,
        variance: f32,
        candidate_idx: usize,
        epsilon_pick: Option<usize>,
    ) -> f32 {
        match self.runtime_config.acquisition {
            GpAcquisitionStrategy::Ucb => ucb_score(mean, variance, self.runtime_config.ucb_kappa),
            GpAcquisitionStrategy::Thompson => {
                let mut rng = rand::thread_rng();
                thompson_sample(mean, variance, &mut rng)
            }
            GpAcquisitionStrategy::Greedy => mean,
            GpAcquisitionStrategy::EpsilonGreedy => {
                if epsilon_pick == Some(candidate_idx) {
                    mean + 2.0
                } else {
                    mean
                }
            }
        }
    }
}

fn prompt_chars(request: &AnthropicRequest) -> usize {
    let message_bytes = request
        .messages
        .iter()
        .map(|message| message.content.to_string().len())
        .sum::<usize>();
    let system_bytes = request
        .system
        .as_ref()
        .map_or(0, |system| system.to_string().len());
    message_bytes + system_bytes
}

fn classify_request(request: &AnthropicRequest) -> u8 {
    let streaming = request.stream.unwrap_or(false);
    let tool_heavy = request
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
        || request
            .messages
            .iter()
            .any(|message| message.role == "tool");

    match (streaming, tool_heavy) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    }
}

fn normalize_capacity(max_streams: usize, active_streams: usize) -> usize {
    if max_streams == 0 {
        active_streams.saturating_add(1).max(1)
    } else {
        max_streams.max(1)
    }
}

/// Approximate input tokens before dispatch. This matches the existing CCR
/// fallback convention of four UTF-8 bytes per token and is used only for
/// relative cost ranking; authoritative usage still comes from the provider.
fn estimate_input_tokens(prompt_chars: usize) -> u64 {
    (prompt_chars.saturating_add(3) / 4) as u64
}

fn estimate_tier_costs(
    ordered: &[(String, String)],
    request: &RequestFeatureContext,
    config: &Config,
) -> HashMap<String, TierCostEstimate> {
    let absolute = ordered
        .iter()
        .map(|(tier, _)| {
            let usd = tier
                .split_once(',')
                .and_then(|(_, model)| {
                    config
                        .resolve_provider(tier)
                        .and_then(|provider| provider.pricing_for_model(model))
                })
                .and_then(|pricing| {
                    pricing.estimate_request_cost_usd(
                        request.estimated_input_tokens,
                        request.max_output_tokens,
                    )
                });
            (tier.clone(), usd)
        })
        .collect::<Vec<_>>();

    let max_cost = absolute
        .iter()
        .filter_map(|(_, cost)| *cost)
        .fold(0.0_f64, f64::max);

    absolute
        .into_iter()
        .map(|(tier, usd)| {
            let relative = usd.map(|cost| {
                if max_cost > 0.0 {
                    (cost / max_cost).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                }
            });
            (tier, TierCostEstimate { usd, relative })
        })
        .collect()
}

/// Apply a quality-credible-set then cost-lexicographic ordering.
///
/// The best posterior lower confidence bound defines the quality bar. Any
/// candidate whose upper confidence bound overlaps that bar remains credible;
/// only within that statistically indistinguishable set do we prefer lower
/// estimated cost. This avoids an arbitrary scalar exchange rate between model
/// quality and dollars. If any credible candidate is unpriced, the entire set
/// retains acquisition ordering so missing metadata never masquerades as free.
fn credible_set_cost_order(candidates: &mut [ScoredCandidate], kappa: f32) {
    if candidates.is_empty() {
        return;
    }

    let confidence = kappa.max(0.0);
    let best_lower_bound = candidates
        .iter()
        .map(|candidate| candidate.mean - confidence * candidate.variance.max(0.0).sqrt())
        .fold(f32::NEG_INFINITY, f32::max);

    for candidate in candidates.iter_mut() {
        let upper_bound = candidate.mean + confidence * candidate.variance.max(0.0).sqrt();
        candidate.credible = upper_bound >= best_lower_bound;
    }

    let credible_count = candidates
        .iter()
        .filter(|candidate| candidate.credible)
        .count();
    let cost_comparable = credible_count > 1
        && candidates
            .iter()
            .filter(|candidate| candidate.credible)
            .all(|candidate| candidate.estimated_cost_usd.is_some());

    candidates.sort_by(|left, right| {
        right
            .credible
            .cmp(&left.credible)
            .then_with(|| {
                if left.credible && right.credible && cost_comparable {
                    left.estimated_cost_usd
                        .expect("cost-comparable candidates are priced")
                        .partial_cmp(
                            &right
                                .estimated_cost_usd
                                .expect("cost-comparable candidates are priced"),
                        )
                        .unwrap_or(CmpOrdering::Equal)
                } else {
                    right
                        .acquisition_score
                        .partial_cmp(&left.acquisition_score)
                        .unwrap_or(CmpOrdering::Equal)
                }
            })
            .then_with(|| {
                right
                    .acquisition_score
                    .partial_cmp(&left.acquisition_score)
                    .unwrap_or(CmpOrdering::Equal)
            })
            .then_with(|| left.canonical_index.cmp(&right.canonical_index))
    });
}

fn outcome_score(duration_secs: Option<f64>) -> f32 {
    match duration_secs {
        Some(duration) if duration.is_finite() && duration >= 0.0 => {
            (1.0 / (1.0 + duration as f32)).clamp(0.0, 1.0)
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn minimal_config() -> Config {
        let raw = json!({
            "Providers": [{
                "name": "mock",
                "api_base_url": "http://localhost:9999/v1/chat/completions",
                "api_key": "x",
                "models": ["m"]
            }],
            "Router": {
                "default": "mock,m",
                "tiers": ["mock,m"]
            }
        });
        let temp = tempfile::NamedTempFile::new().expect("temp config file");
        fs::write(
            temp.path(),
            serde_json::to_vec(&raw).expect("serialize config"),
        )
        .expect("write config file");
        Config::from_file(temp.path().to_str().expect("config path"))
            .expect("load Config from file")
    }

    fn sample_request() -> AnthropicRequest {
        AnthropicRequest {
            model: "mock,m".to_string(),
            messages: vec![crate::router::Message {
                role: "user".to_string(),
                content: serde_json::Value::String("hello world".to_string()),
                tool_call_id: None,
            }],
            system: None,
            max_tokens: Some(64),
            temperature: Some(0.2),
            stream: Some(false),
            tools: None,
            openai_passthrough_body: None,
        }
    }

    #[test]
    fn unfitted_router_keeps_original_order() {
        let router = GpRequestRouter::new(
            GpRoutingRuntimeConfig {
                enabled: true,
                ..GpRoutingRuntimeConfig::default()
            },
            &["mock,m".to_string()],
        );
        let config = minimal_config();
        let ordered = vec![("mock,m".to_string(), "mock".to_string())];
        let plan = router.plan_rerank(&ordered, &sample_request(), &config, 0, 16, 0);
        assert_eq!(plan.ordered, ordered);
    }

    #[test]
    fn failure_observations_map_to_zero_score() {
        assert_eq!(outcome_score(None), 0.0);
        assert_eq!(outcome_score(Some(-1.0)), 0.0);
        assert!(outcome_score(Some(0.5)) > outcome_score(Some(3.0)));
    }

    #[test]
    fn pricing_math_uses_input_and_output_rates() {
        let pricing = crate::config::ModelPricing {
            input_per_million_tokens: 1.5,
            output_per_million_tokens: 6.0,
        };

        let cost = pricing
            .estimate_request_cost_usd(2_000, 500)
            .expect("valid pricing");
        assert!((cost - 0.006).abs() < f64::EPSILON);
    }

    #[test]
    fn gp_encoder_covers_thirty_two_candidates_and_leaves_tail_in_fallback() {
        let tiers = (0..33)
            .map(|idx| format!("mock,m{idx}"))
            .collect::<Vec<_>>();
        let router = GpRequestRouter::new(GpRoutingRuntimeConfig::default(), &tiers);

        assert_eq!(router.backend_indices.len(), 32);
        assert_eq!(router.model_config.n_backends, 32);
        let request_features = RequestFeatureContext::from_request(&sample_request(), 0, 16);
        let features = router
            .build_features(&request_features, "mock,m31", 0, Some(0.5))
            .expect("last bounded backend has a feature slot");
        let tail_index = gp_routing::features::BACKEND_START + 31;
        assert_eq!(features.as_array()[tail_index], 1.0);
        assert!(router
            .build_features(&request_features, "mock,m32", 0, Some(0.5))
            .is_none());
    }

    #[test]
    fn relative_costs_are_continuous_per_request() {
        let raw = json!({
            "Providers": [{
                "name": "mock",
                "api_base_url": "https://example.test/v1",
                "api_key": "x",
                "models": ["cheap", "expensive"],
                "pricing": {
                    "input_per_million_tokens": 1.0,
                    "output_per_million_tokens": 2.0
                },
                "model_pricing": {
                    "expensive": {
                        "input_per_million_tokens": 10.0,
                        "output_per_million_tokens": 20.0
                    }
                }
            }],
            "Router": {
                "default": "mock,cheap",
                "tiers": ["mock,cheap", "mock,expensive"]
            }
        });
        let temp = tempfile::NamedTempFile::new().expect("temp config file");
        fs::write(
            temp.path(),
            serde_json::to_vec(&raw).expect("serialize config"),
        )
        .expect("write config file");
        let config =
            Config::from_file(temp.path().to_str().expect("config path")).expect("load config");
        let ordered = vec![
            ("mock,cheap".to_string(), "cheap".to_string()),
            ("mock,expensive".to_string(), "expensive".to_string()),
        ];
        let request = RequestFeatureContext::from_request(&sample_request(), 0, 16);
        let costs = estimate_tier_costs(&ordered, &request, &config);

        assert!((costs["mock,cheap"].relative.expect("priced") - 0.1).abs() < 1e-6);
        assert_eq!(costs["mock,expensive"].relative, Some(1.0));
        assert!(costs["mock,cheap"].usd < costs["mock,expensive"].usd);
    }

    #[test]
    fn cost_orders_candidates_only_inside_quality_credible_set() {
        let mut candidates = vec![
            ScoredCandidate {
                canonical_index: 0,
                entry: ("mock,expensive".to_string(), "expensive".to_string()),
                acquisition_score: 0.81,
                mean: 0.70,
                variance: 0.04,
                estimated_cost_usd: Some(0.10),
                credible: false,
            },
            ScoredCandidate {
                canonical_index: 1,
                entry: ("mock,cheap".to_string(), "cheap".to_string()),
                acquisition_score: 0.79,
                mean: 0.69,
                variance: 0.04,
                estimated_cost_usd: Some(0.001),
                credible: false,
            },
        ];

        credible_set_cost_order(&mut candidates, 2.0);
        assert_eq!(candidates[0].entry.1, "cheap");

        candidates[0].mean = 0.20;
        candidates[0].variance = 0.0001;
        candidates[0].acquisition_score = 0.22;
        candidates[1].mean = 0.90;
        candidates[1].variance = 0.0001;
        candidates[1].acquisition_score = 0.92;
        credible_set_cost_order(&mut candidates, 2.0);
        assert_eq!(candidates[0].entry.1, "expensive");
    }

    #[test]
    fn mixed_pricing_preserves_acquisition_order_inside_credible_set() {
        let mut candidates = vec![
            ScoredCandidate {
                canonical_index: 0,
                entry: ("mock,priced".to_string(), "priced".to_string()),
                acquisition_score: 0.81,
                mean: 0.70,
                variance: 0.04,
                estimated_cost_usd: Some(0.10),
                credible: false,
            },
            ScoredCandidate {
                canonical_index: 1,
                entry: ("mock,unknown".to_string(), "unknown".to_string()),
                acquisition_score: 0.79,
                mean: 0.69,
                variance: 0.04,
                estimated_cost_usd: None,
                credible: false,
            },
        ];

        credible_set_cost_order(&mut candidates, 2.0);
        assert_eq!(candidates[0].entry.1, "priced");
        assert!(candidates.iter().all(|candidate| candidate.credible));
    }
}
