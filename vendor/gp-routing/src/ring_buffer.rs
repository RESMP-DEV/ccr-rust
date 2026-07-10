// Modified for CCR-Rust: rustfmt normalization only; behavior is unchanged.
use crate::features::{FeatureVector, FEATURE_DIM};
use ndarray::{Array1, Array2};
use tracing::{debug, info};

/// Fixed-capacity observation buffer with FIFO overwrite semantics.
#[derive(Clone, Debug)]
pub struct ObservationBuffer {
    entries: Vec<(Array1<f32>, f32)>,
    capacity: usize,
    write_cursor: usize,
    requests_since_last_fit: usize,
}

impl ObservationBuffer {
    /// Create a new observation buffer.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        info!(capacity, "observation buffer created");
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
            write_cursor: 0,
            requests_since_last_fit: 0,
        }
    }

    /// Push a new observation, evicting the oldest when full.
    pub fn push(&mut self, features: FeatureVector, outcome: f32) {
        debug!(
            outcome,
            len = self.entries.len(),
            capacity = self.capacity,
            "pushing observation"
        );
        let clamped_outcome = outcome.clamp(0.0, 1.0);
        let record = (features.as_array().clone(), clamped_outcome);

        if self.entries.len() < self.capacity {
            self.entries.push(record);
            self.write_cursor = self.entries.len() % self.capacity;
        } else {
            self.entries[self.write_cursor] = record;
            self.write_cursor = (self.write_cursor + 1) % self.capacity;
        }

        self.requests_since_last_fit = self.requests_since_last_fit.saturating_add(1);
    }

    /// Number of retained observations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Configured capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Return training arrays `(X, y)` in insertion order.
    #[must_use]
    pub fn as_training_data(&self) -> (Array2<f32>, Array1<f32>) {
        let len = self.entries.len();
        if len == 0 {
            return (Array2::zeros((0, FEATURE_DIM)), Array1::zeros(0));
        }

        let mut x = Array2::zeros((len, FEATURE_DIM));
        let mut y = Array1::zeros(len);

        if len < self.capacity {
            for (row, (features, outcome)) in self.entries.iter().enumerate() {
                x.row_mut(row).assign(features);
                y[row] = *outcome;
            }
            return (x, y);
        }

        for row in 0..len {
            let idx = (self.write_cursor + row) % self.capacity;
            let (features, outcome) = &self.entries[idx];
            x.row_mut(row).assign(features);
            y[row] = *outcome;
        }

        (x, y)
    }

    /// Remove all observations.
    pub fn clear(&mut self) {
        let prev_len = self.entries.len();
        self.entries.clear();
        self.write_cursor = 0;
        self.requests_since_last_fit = 0;
        info!(prev_len, "observation buffer cleared");
    }

    /// Number of pushes since the last successful fit marker.
    #[must_use]
    pub fn requests_since_last_fit(&self) -> usize {
        self.requests_since_last_fit
    }

    /// Reset the fit counter after a successful model refresh.
    pub fn mark_fitted(&mut self) {
        self.requests_since_last_fit = 0;
    }
}
