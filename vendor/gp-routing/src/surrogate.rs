use crate::config::GpRoutingConfig;
use crate::features::FeatureVector;
use crate::ring_buffer::ObservationBuffer;
use egobox_gp::correlation_models::Matern52Corr;
use egobox_gp::mean_models::ConstantMean;
use egobox_gp::{GaussianProcess, ThetaTuning};
use linfa::dataset::Dataset;
use linfa::prelude::Fit;
use ndarray::{Array1, Axis};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

type RoutingModel = GaussianProcess<f32, ConstantMean, Matern52Corr>;

#[derive(Debug, Serialize, Deserialize)]
struct PersistedState {
    input_dim: usize,
    fit_count: u64,
    last_fit_duration_ms: u64,
    y_stats: Option<(f32, f32)>,
    model: RoutingModel,
}

/// Errors returned by [`GpSurrogate::fit`].
#[derive(Debug, Clone, PartialEq)]
pub enum GpFitError {
    /// Not enough observations are available to train the GP.
    InsufficientData { have: usize, need: usize },
    /// Fitting failed inside `egobox-gp` or `linfa`.
    FitFailed(String),
}

impl Display for GpFitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsufficientData { have, need } => {
                write!(f, "insufficient data for GP fit: have {have}, need {need}")
            }
            Self::FitFailed(message) => write!(f, "GP fit failed: {message}"),
        }
    }
}

impl Error for GpFitError {}

/// Errors returned by [`GpSurrogate::save`].
#[derive(Debug)]
pub enum GpSaveError {
    /// Attempted to save an unfitted model.
    NotFitted,
    /// File system error during save.
    Io(std::io::Error),
    /// JSON serialization error during save.
    Serde(serde_json::Error),
}

impl Display for GpSaveError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFitted => write!(f, "cannot save an unfitted GP model"),
            Self::Io(err) => write!(f, "failed to write GP model: {err}"),
            Self::Serde(err) => write!(f, "failed to serialize GP model: {err}"),
        }
    }
}

impl Error for GpSaveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotFitted => None,
            Self::Io(err) => Some(err),
            Self::Serde(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for GpSaveError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for GpSaveError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}

/// Errors returned by [`GpSurrogate::load`].
#[derive(Debug)]
pub enum GpLoadError {
    /// File system error during load.
    Io(std::io::Error),
    /// JSON deserialization error during load.
    Serde(serde_json::Error),
    /// Persisted feature dimension does not match current configuration.
    DimensionMismatch { expected: usize, found: usize },
}

impl Display for GpLoadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "failed to read GP model: {err}"),
            Self::Serde(err) => write!(f, "failed to deserialize GP model: {err}"),
            Self::DimensionMismatch { expected, found } => write!(
                f,
                "GP model dimension mismatch: expected {expected}, found {found}"
            ),
        }
    }
}

impl Error for GpLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Serde(err) => Some(err),
            Self::DimensionMismatch { .. } => None,
        }
    }
}

impl From<std::io::Error> for GpLoadError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for GpLoadError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}

/// Thread-safe wrapper around an `egobox-gp` Gaussian process.
#[derive(Clone, Debug)]
pub struct GpSurrogate {
    model: Arc<RwLock<Option<RoutingModel>>>,
    config: GpRoutingConfig,
    fit_count: Arc<AtomicU64>,
    last_fit_duration_ms: Arc<AtomicU64>,
    y_stats: Arc<RwLock<Option<(f32, f32)>>>,
}

impl GpSurrogate {
    /// Construct an unfitted GP surrogate.
    #[must_use]
    pub fn new(config: GpRoutingConfig) -> Self {
        debug!(input_dim = config.input_dim, nugget = config.nugget, kpls_dim = ?config.kpls_dim, "creating GP surrogate");
        Self {
            model: Arc::new(RwLock::new(None)),
            config,
            fit_count: Arc::new(AtomicU64::new(0)),
            last_fit_duration_ms: Arc::new(AtomicU64::new(0)),
            y_stats: Arc::new(RwLock::new(None)),
        }
    }

    /// Whether a fitted model is currently available.
    #[must_use]
    pub fn is_fitted(&self) -> bool {
        self.model.read().is_some()
    }

    /// Number of successful fits performed by this surrogate.
    #[must_use]
    pub fn fit_count(&self) -> u64 {
        self.fit_count.load(Ordering::Relaxed)
    }

    /// Duration of the last fit in milliseconds.
    #[must_use]
    pub fn last_fit_duration_ms(&self) -> u64 {
        self.last_fit_duration_ms.load(Ordering::Relaxed)
    }

    fn prior_pair(&self) -> (f32, f32) {
        (self.config.prior_mean, self.config.prior_variance)
    }

    /// Fit a GP model from the observation buffer.
    pub fn fit(&self, buffer: &ObservationBuffer) -> Result<(), GpFitError> {
        let (x, y) = buffer.as_training_data();
        let have = y.len();
        if have < self.config.min_observations {
            return Err(GpFitError::InsufficientData {
                have,
                need: self.config.min_observations,
            });
        }

        let start = Instant::now();
        let y_min = y.iter().copied().fold(f32::INFINITY, f32::min);
        let y_max = y.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let y_span = (y_max - y_min).max(1e-8);
        let y_norm: Array1<f32> = y.mapv(|value| ((value - y_min) / y_span).clamp(0.0, 1.0));

        let dataset = Dataset::new(x, y_norm);
        let theta_dim = self
            .config
            .kpls_dim
            .unwrap_or(self.config.input_dim)
            .max(1)
            .min(self.config.input_dim.max(1));
        let theta = Array1::from_elem(theta_dim, 1e-1_f32);

        // egobox-gp 0.36.x optimizes theta through a generic helper that
        // unsafely reinterprets `f32` as `f64`, which aborts on macOS/ARM with a
        // misaligned pointer dereference. Using fixed theta keeps the FP32 path
        // stable while still giving us a functional ARD-capable GP wrapper.
        let params = GaussianProcess::<f32, ConstantMean, Matern52Corr>::params(
            ConstantMean::default(),
            Matern52Corr::default(),
        )
        .nugget(self.config.nugget)
        .theta_tuning(ThetaTuning::Fixed(theta))
        .kpls_dim(self.config.kpls_dim);

        let model = params
            .fit(&dataset)
            .map_err(|err| GpFitError::FitFailed(err.to_string()))?;

        let duration_ms = start.elapsed().as_millis() as u64;
        *self.model.write() = Some(model);
        *self.y_stats.write() = Some((y_min, y_max));
        self.fit_count.fetch_add(1, Ordering::Relaxed);
        self.last_fit_duration_ms
            .store(duration_ms, Ordering::Relaxed);

        info!(
            observations = have,
            duration_ms,
            y_min,
            y_max,
            kpls_dim = ?self.config.kpls_dim,
            "gp-routing fit complete"
        );
        Ok(())
    }

    /// Predict mean and variance for a single feature vector.
    #[must_use]
    pub fn predict(&self, features: &FeatureVector) -> (f32, f32) {
        let guard = self.model.read();
        let Some(model) = guard.as_ref() else {
            return self.prior_pair();
        };

        let input = features.as_array().clone().insert_axis(Axis(0));
        match model.predict_valvar(&input) {
            Ok((means, variances)) => {
                let mean = means.get(0).copied().unwrap_or(self.config.prior_mean);
                let variance = variances
                    .get(0)
                    .copied()
                    .unwrap_or(self.config.prior_variance)
                    .max(1e-6);
                (mean, variance)
            }
            Err(err) => {
                warn!(error = %err, "gp-routing single prediction failed; using prior");
                self.prior_pair()
            }
        }
    }

    /// Predict mean and variance for a batch of feature vectors.
    #[must_use]
    pub fn predict_batch(&self, features: &[FeatureVector]) -> Vec<(f32, f32)> {
        debug!(batch_size = features.len(), "gp-routing batch prediction");
        if features.is_empty() {
            return Vec::new();
        }

        let guard = self.model.read();
        let Some(model) = guard.as_ref() else {
            return vec![self.prior_pair(); features.len()];
        };

        let mut inputs = ndarray::Array2::zeros((features.len(), self.config.input_dim));
        for (row, feature) in features.iter().enumerate() {
            inputs.row_mut(row).assign(feature.as_array());
        }

        match model.predict_valvar(&inputs) {
            Ok((means, variances)) => means
                .iter()
                .zip(variances.iter())
                .map(|(mean, variance)| (*mean, (*variance).max(1e-6)))
                .collect(),
            Err(err) => {
                warn!(error = %err, "gp-routing batch prediction failed; using priors");
                vec![self.prior_pair(); features.len()]
            }
        }
    }

    /// Persist the fitted model to disk as JSON.
    pub fn save(&self, path: &Path) -> Result<(), GpSaveError> {
        debug!(path = %path.display(), "saving GP model to disk");
        let model = self.model.read().clone().ok_or(GpSaveError::NotFitted)?;
        let state = PersistedState {
            input_dim: self.config.input_dim,
            fit_count: self.fit_count(),
            last_fit_duration_ms: self.last_fit_duration_ms(),
            y_stats: *self.y_stats.read(),
            model,
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let tmp_path = path.with_extension("tmp");
        let payload = serde_json::to_vec_pretty(&state)?;
        std::fs::write(&tmp_path, payload)?;
        std::fs::rename(&tmp_path, path)?;
        info!(path = %path.display(), "GP model saved to disk");
        Ok(())
    }

    /// Load a previously persisted model into this surrogate.
    pub fn load(&self, path: &Path) -> Result<(), GpLoadError> {
        debug!(path = %path.display(), "loading GP model from disk");
        let payload = std::fs::read(path)?;
        let state: PersistedState = serde_json::from_slice(&payload)?;
        if state.input_dim != self.config.input_dim {
            return Err(GpLoadError::DimensionMismatch {
                expected: self.config.input_dim,
                found: state.input_dim,
            });
        }

        let (model_input_dim, _) = state.model.dims();
        if model_input_dim != self.config.input_dim {
            return Err(GpLoadError::DimensionMismatch {
                expected: self.config.input_dim,
                found: model_input_dim,
            });
        }

        *self.model.write() = Some(state.model);
        *self.y_stats.write() = state.y_stats;
        self.fit_count.store(state.fit_count, Ordering::Relaxed);
        self.last_fit_duration_ms
            .store(state.last_fit_duration_ms, Ordering::Relaxed);

        info!(path = %path.display(), "gp-routing model loaded from disk");
        Ok(())
    }
}
