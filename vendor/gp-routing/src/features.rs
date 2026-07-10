// Modified for CCR-Rust: support 32 backend slots and encode continuous
// relative request cost in place of the upstream paid/free bit.
use ndarray::Array1;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;
use tracing::debug;

pub const PROMPT_LEN_INDEX: usize = 0;
pub const PRIORITY_START: usize = 1;
pub const PRIORITY_SLOTS: usize = 4;
pub const HAS_VERIFY_INDEX: usize = 5;
pub const DEP_COUNT_INDEX: usize = 6;
pub const RETRY_COUNT_INDEX: usize = 7;
pub const BACKEND_START: usize = 8;
/// One-hot capacity for configured provider/model routes.
///
/// Thirty-two slots provide bounded growth room for larger routing catalogs
/// while keeping the GP input dimension fixed.
pub const BACKEND_SLOTS: usize = 32;
pub const IDLE_RATIO_INDEX: usize = BACKEND_START + BACKEND_SLOTS;
pub const IN_FLIGHT_RATIO_INDEX: usize = IDLE_RATIO_INDEX + 1;
pub const HOUR_SIN_INDEX: usize = IN_FLIGHT_RATIO_INDEX + 1;
pub const HOUR_COS_INDEX: usize = HOUR_SIN_INDEX + 1;
pub const LOOP_COUNT_INDEX: usize = HOUR_COS_INDEX + 1;
pub const RELATIVE_COST_INDEX: usize = LOOP_COUNT_INDEX + 1;
pub const COST_KNOWN_INDEX: usize = RELATIVE_COST_INDEX + 1;
pub const FEATURE_DIM: usize = COST_KNOWN_INDEX + 1;

fn clip_unit(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// Fixed-size routing feature vector.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FeatureVector(Array1<f32>);

impl FeatureVector {
    /// Start a builder using zero-initialized features.
    #[must_use]
    pub fn builder() -> FeatureVectorBuilder {
        FeatureVectorBuilder::default()
    }

    /// Borrow the underlying feature array.
    #[must_use]
    pub fn as_array(&self) -> &Array1<f32> {
        &self.0
    }

    /// Return the static feature dimension.
    #[must_use]
    pub fn dim() -> usize {
        FEATURE_DIM
    }

    /// Clone this feature vector and update only the backend one-hot slice.
    #[must_use]
    pub fn with_backend_index(&self, idx: usize, n_backends: usize) -> Self {
        let mut data = self.0.clone();
        for slot in BACKEND_START..(BACKEND_START + BACKEND_SLOTS) {
            data[slot] = 0.0;
        }
        if idx < n_backends && idx < BACKEND_SLOTS {
            data[BACKEND_START + idx] = 1.0;
        }
        Self(data)
    }
}

/// Builder for [`FeatureVector`].
#[derive(Clone, Debug, Default)]
pub struct FeatureVectorBuilder {
    prompt_len: f32,
    priority_0: f32,
    priority_1: f32,
    priority_2: f32,
    priority_3: f32,
    has_verify: f32,
    dep_count: f32,
    retry_count: f32,
    backend: [f32; BACKEND_SLOTS],
    idle_ratio: f32,
    in_flight_ratio: f32,
    hour_sin: f32,
    hour_cos: f32,
    loop_count: f32,
    relative_cost: f32,
    cost_known: f32,
}

impl FeatureVectorBuilder {
    /// Log-scale prompt length and clip it to the unit interval.
    #[must_use]
    pub fn prompt_length(mut self, chars: usize) -> Self {
        let value = (usize::max(chars, 1) as f32).ln() / 18.0;
        self.prompt_len = clip_unit(value);
        self
    }

    /// One-hot encode a priority in the range `0..=3`.
    #[must_use]
    pub fn priority(mut self, p: u8) -> Self {
        self.priority_0 = 0.0;
        self.priority_1 = 0.0;
        self.priority_2 = 0.0;
        self.priority_3 = 0.0;
        match p {
            0 => self.priority_0 = 1.0,
            1 => self.priority_1 = 1.0,
            2 => self.priority_2 = 1.0,
            3 => self.priority_3 = 1.0,
            _ => {}
        }
        self
    }

    /// Encode whether a task provides an explicit verify command.
    #[must_use]
    pub fn has_verify(mut self, value: bool) -> Self {
        self.has_verify = if value { 1.0 } else { 0.0 };
        self
    }

    /// Normalize dependency count into `[0, 1]`.
    #[must_use]
    pub fn dependency_count(mut self, n: usize) -> Self {
        self.dep_count = clip_unit((n.min(10) as f32) / 10.0);
        self
    }

    /// Normalize retry count into `[0, 1]`.
    #[must_use]
    pub fn retry_count(mut self, n: usize) -> Self {
        self.retry_count = clip_unit((n.min(18) as f32) / 18.0);
        self
    }

    /// One-hot encode the backend index over [`BACKEND_SLOTS`] slots.
    #[must_use]
    pub fn backend_index(mut self, idx: usize, n_backends: usize) -> Self {
        self.backend.fill(0.0);
        if idx < n_backends && idx < BACKEND_SLOTS {
            self.backend[idx] = 1.0;
        }
        self
    }

    /// Encode the ratio of idle workers to total workers.
    #[must_use]
    pub fn idle_ratio(mut self, idle: usize, total: usize) -> Self {
        let total = total.max(1) as f32;
        self.idle_ratio = clip_unit((idle as f32) / total);
        self
    }

    /// Encode the ratio of in-flight tasks to total workers.
    #[must_use]
    pub fn in_flight_ratio(mut self, in_flight: usize, total: usize) -> Self {
        let total = total.max(1) as f32;
        self.in_flight_ratio = clip_unit((in_flight as f32) / total);
        self
    }

    /// Encode hour of day cyclically.
    ///
    /// This preserves circular distance, so the resulting values intentionally
    /// live in `[-1, 1]` instead of `[0, 1]`.
    #[must_use]
    pub fn hour_of_day(mut self, hour: u32) -> Self {
        let angle = ((hour % 24) as f32) * 2.0 * PI / 24.0;
        self.hour_sin = angle.sin();
        self.hour_cos = angle.cos();
        self
    }

    /// Normalize loop count into `[0, 1]`.
    #[must_use]
    pub fn loop_count(mut self, n: usize) -> Self {
        self.loop_count = clip_unit((n.min(3) as f32) / 3.0);
        self
    }

    /// Encode request cost relative to the most expensive priced candidate.
    ///
    /// Callers calculate this ratio per request so the GP receives a bounded,
    /// continuous signal without a currency-specific magic normalization
    /// constant. Unknown pricing is represented by the caller, not here.
    #[must_use]
    pub fn relative_cost(mut self, relative_cost: Option<f32>) -> Self {
        if let Some(relative_cost) = relative_cost {
            self.relative_cost = clip_unit(relative_cost);
            self.cost_known = 1.0;
        } else {
            self.relative_cost = 0.0;
            self.cost_known = 0.0;
        }
        self
    }

    /// Build the feature vector in the canonical fixed-dimensional order.
    #[must_use]
    pub fn build(self) -> FeatureVector {
        let mut values = vec![
            self.prompt_len,
            self.priority_0,
            self.priority_1,
            self.priority_2,
            self.priority_3,
            self.has_verify,
            self.dep_count,
            self.retry_count,
        ];
        values.extend(self.backend);
        values.extend([
            self.idle_ratio,
            self.in_flight_ratio,
            self.hour_sin,
            self.hour_cos,
            self.loop_count,
            self.relative_cost,
            self.cost_known,
        ]);
        debug_assert_eq!(values.len(), FEATURE_DIM);
        debug!(
            prompt_len = self.prompt_len,
            idle_ratio = self.idle_ratio,
            in_flight_ratio = self.in_flight_ratio,
            "feature vector built"
        );
        FeatureVector(Array1::from(values))
    }
}
