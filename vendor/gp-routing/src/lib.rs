//! # gp-routing
//!
//! Gaussian Process surrogate for LLM backend routing and load balancing.
//!
//! Uses Kriging (GP regression) with per-dimension ARD lengthscales to learn
//! which request features predict backend performance. Designed for scenarios
//! where API calls take 10-20 seconds and the GP refit happens every ~200
//! requests, making FP32 precision more than adequate.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use gp_routing::{FeatureVector, GpRoutingConfig, GpSurrogate, ObservationBuffer};
//!
//! let config = GpRoutingConfig::default();
//! let mut buffer = ObservationBuffer::new(config.buffer_capacity);
//! let surrogate = GpSurrogate::new(config.clone());
//!
//! let features = FeatureVector::builder()
//!     .prompt_length(5_000)
//!     .priority(1)
//!     .backend_index(2, config.n_backends)
//!     .hour_of_day(14)
//!     .build();
//! buffer.push(features.clone(), 0.85);
//!
//! if buffer.len() >= config.min_observations {
//!     surrogate.fit(&buffer).expect("GP fit should succeed");
//! }
//!
//! let (_mean, _variance) = surrogate.predict(&features);
//! ```

pub mod acquisition;
pub mod config;
pub mod features;
pub mod ring_buffer;
pub mod surrogate;

pub use acquisition::{rank_backends, thompson_sample, ucb_score, AcquisitionStrategy};
pub use config::{GpRoutingConfig, GpRoutingConfigBuilder};
pub use features::{FeatureVector, FeatureVectorBuilder, FEATURE_DIM};
pub use ring_buffer::ObservationBuffer;
pub use surrogate::{GpFitError, GpLoadError, GpSaveError, GpSurrogate};
