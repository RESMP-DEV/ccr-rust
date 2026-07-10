// Modified for CCR-Rust: rustfmt normalization only; behavior is unchanged.
use crate::features::FeatureVector;
use crate::surrogate::GpSurrogate;
use rand::Rng;
use std::cmp::Ordering;
use std::f32::consts::TAU;
use tracing::{debug, info};

/// Strategies for turning GP posteriors into routing scores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AcquisitionStrategy {
    /// Upper confidence bound.
    Ucb { kappa: f32 },
    /// Thompson sampling with one posterior draw per backend.
    Thompson,
    /// Pure exploitation.
    Greedy,
    /// Greedy with occasional random promotion.
    EpsilonGreedy { epsilon: f32 },
}

/// Upper confidence bound score.
#[must_use]
pub fn ucb_score(mean: f32, variance: f32, kappa: f32) -> f32 {
    mean + kappa.max(0.0) * variance.max(0.0).sqrt()
}

/// Draw a Thompson sample from `N(mean, variance)`.
#[must_use]
pub fn thompson_sample(mean: f32, variance: f32, rng: &mut impl rand::Rng) -> f32 {
    let variance = variance.max(0.0);
    if variance == 0.0 {
        return mean;
    }

    let u1 = rng.gen::<f32>().max(f32::MIN_POSITIVE);
    let u2 = rng.gen::<f32>();
    let z0 = (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos();
    mean + variance.sqrt() * z0
}

/// Score every backend by mutating the backend one-hot slice of `base_features`.
#[must_use]
pub fn rank_backends(
    surrogate: &GpSurrogate,
    base_features: &FeatureVector,
    n_backends: usize,
    strategy: AcquisitionStrategy,
) -> Vec<(usize, f32)> {
    debug!(n_backends, ?strategy, "ranking backends");
    if n_backends == 0 {
        return Vec::new();
    }

    let mut rng = rand::thread_rng();
    let epsilon_pick = match strategy {
        AcquisitionStrategy::EpsilonGreedy { epsilon } => {
            let epsilon = epsilon.clamp(0.0, 1.0);
            if rng.gen::<f32>() < epsilon {
                Some(rng.gen_range(0..n_backends))
            } else {
                None
            }
        }
        _ => None,
    };

    let mut scored = Vec::with_capacity(n_backends);
    for backend_idx in 0..n_backends {
        let features = base_features.with_backend_index(backend_idx, n_backends);
        let (mean, variance) = surrogate.predict(&features);
        let score = match strategy {
            AcquisitionStrategy::Ucb { kappa } => ucb_score(mean, variance, kappa),
            AcquisitionStrategy::Thompson => thompson_sample(mean, variance, &mut rng),
            AcquisitionStrategy::Greedy => mean,
            AcquisitionStrategy::EpsilonGreedy { .. } => {
                if epsilon_pick == Some(backend_idx) {
                    mean + 2.0
                } else {
                    mean
                }
            }
        };
        scored.push((backend_idx, score));
    }

    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    if let Some((best_idx, best_score)) = scored.first() {
        info!(
            best_backend = best_idx,
            best_score,
            n_backends,
            ?strategy,
            "backend ranking complete"
        );
    }
    scored
}
