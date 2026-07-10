// Modified for CCR-Rust: derive the input dimension from the expanded local
// feature layout instead of the upstream 22-dimensional prototype constant.
use crate::features::FEATURE_DIM;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Configuration for the GP routing surrogate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GpRoutingConfig {
    /// Number of input features in the feature vector.
    /// Default: [`FEATURE_DIM`].
    pub input_dim: usize,
    /// Maximum observations to retain in the ring buffer.
    pub buffer_capacity: usize,
    /// Minimum observations before GP fitting is attempted.
    pub min_observations: usize,
    /// Number of requests between automatic refits. `0` disables auto-refit.
    pub refit_interval: usize,
    /// UCB exploration weight.
    pub ucb_kappa: f32,
    /// Nugget (jitter) for numerical stability.
    pub nugget: f32,
    /// Optional KPLS dimension reduction.
    pub kpls_dim: Option<usize>,
    /// Prior mean returned when the GP is unfitted.
    pub prior_mean: f32,
    /// Prior variance returned when the GP is unfitted.
    pub prior_variance: f32,
    /// Number of backends in the target router.
    pub n_backends: usize,
}

impl Default for GpRoutingConfig {
    fn default() -> Self {
        Self {
            input_dim: FEATURE_DIM,
            buffer_capacity: 500,
            min_observations: 40,
            refit_interval: 200,
            ucb_kappa: 2.0,
            nugget: 1e-4,
            kpls_dim: None,
            prior_mean: 0.5,
            prior_variance: 0.25,
            n_backends: 11,
        }
    }
}

impl GpRoutingConfig {
    /// Create a builder seeded with [`Default`] values.
    #[must_use]
    pub fn builder() -> GpRoutingConfigBuilder {
        GpRoutingConfigBuilder::default()
    }
}

/// Builder for [`GpRoutingConfig`].
#[derive(Debug, Clone)]
pub struct GpRoutingConfigBuilder {
    config: GpRoutingConfig,
}

impl Default for GpRoutingConfigBuilder {
    fn default() -> Self {
        Self {
            config: GpRoutingConfig::default(),
        }
    }
}

impl GpRoutingConfigBuilder {
    #[must_use]
    pub fn input_dim(mut self, input_dim: usize) -> Self {
        self.config.input_dim = input_dim;
        self
    }

    #[must_use]
    pub fn buffer_capacity(mut self, buffer_capacity: usize) -> Self {
        self.config.buffer_capacity = buffer_capacity;
        self
    }

    #[must_use]
    pub fn min_observations(mut self, min_observations: usize) -> Self {
        self.config.min_observations = min_observations;
        self
    }

    #[must_use]
    pub fn refit_interval(mut self, refit_interval: usize) -> Self {
        self.config.refit_interval = refit_interval;
        self
    }

    #[must_use]
    pub fn ucb_kappa(mut self, ucb_kappa: f32) -> Self {
        self.config.ucb_kappa = ucb_kappa;
        self
    }

    #[must_use]
    pub fn nugget(mut self, nugget: f32) -> Self {
        self.config.nugget = nugget;
        self
    }

    #[must_use]
    pub fn kpls_dim(mut self, kpls_dim: Option<usize>) -> Self {
        self.config.kpls_dim = kpls_dim;
        self
    }

    #[must_use]
    pub fn prior_mean(mut self, prior_mean: f32) -> Self {
        self.config.prior_mean = prior_mean;
        self
    }

    #[must_use]
    pub fn prior_variance(mut self, prior_variance: f32) -> Self {
        self.config.prior_variance = prior_variance;
        self
    }

    #[must_use]
    pub fn n_backends(mut self, n_backends: usize) -> Self {
        self.config.n_backends = n_backends;
        self
    }

    #[must_use]
    pub fn build(self) -> GpRoutingConfig {
        info!(
            n_backends = self.config.n_backends,
            buffer_capacity = self.config.buffer_capacity,
            min_observations = self.config.min_observations,
            refit_interval = self.config.refit_interval,
            kpls_dim = ?self.config.kpls_dim,
            "gp-routing config built"
        );
        self.config
    }
}
