// Modified for CCR-Rust: exercise the expanded backend and continuous-cost
// feature contract used by the embedded router.
use approx::assert_abs_diff_eq;
use gp_routing::features::{DEP_COUNT_INDEX, HOUR_COS_INDEX, HOUR_SIN_INDEX};
use gp_routing::{
    rank_backends, AcquisitionStrategy, FeatureVector, GpRoutingConfig, GpSurrogate,
    ObservationBuffer, FEATURE_DIM,
};

fn make_features(backend_idx: usize, sample: usize) -> FeatureVector {
    FeatureVector::builder()
        .prompt_length(1_000 + sample * 137)
        .priority((sample % 4) as u8)
        .has_verify(sample % 2 == 0)
        .dependency_count(sample % 10)
        .retry_count(sample % 7)
        .backend_index(backend_idx, 2)
        .idle_ratio(3 + (sample % 4), 10)
        .in_flight_ratio(2 + (sample % 5), 10)
        .hour_of_day((sample % 24) as u32)
        .loop_count(sample % 3)
        .relative_cost(Some(if backend_idx == 0 { 0.0 } else { 1.0 }))
        .build()
}

fn train_two_backend_surrogate() -> GpSurrogate {
    let config = GpRoutingConfig::builder()
        .buffer_capacity(128)
        .min_observations(20)
        .n_backends(2)
        .build();
    let surrogate = GpSurrogate::new(config);
    let mut buffer = ObservationBuffer::new(128);

    for sample in 0..80 {
        let backend = sample % 2;
        let outcome = if backend == 0 { 0.9 } else { 0.3 };
        buffer.push(make_features(backend, sample), outcome);
    }

    surrogate.fit(&buffer).expect("surrogate should fit");
    surrogate
}

#[test]
fn test_feature_vector_dimensions() {
    let features = FeatureVector::builder()
        .prompt_length(5_000)
        .priority(2)
        .has_verify(true)
        .dependency_count(4)
        .retry_count(3)
        .backend_index(1, 4)
        .idle_ratio(5, 8)
        .in_flight_ratio(2, 8)
        .hour_of_day(14)
        .loop_count(2)
        .relative_cost(Some(1.0))
        .build();

    let values = features.as_array();
    assert_eq!(values.len(), FEATURE_DIM);
    for (idx, value) in values.iter().enumerate() {
        if idx == HOUR_SIN_INDEX || idx == HOUR_COS_INDEX {
            assert!((-1.0..=1.0).contains(value));
        } else {
            assert!((0.0..=1.0).contains(value));
        }
    }
}

#[test]
fn test_backend_capacity_covers_all_thirty_two_slots() {
    let features = FeatureVector::builder()
        .backend_index(31, 32)
        .relative_cost(Some(0.25))
        .build();

    assert_eq!(gp_routing::features::BACKEND_SLOTS, 32);
    assert_eq!(
        features.as_array()[gp_routing::features::BACKEND_START + 31],
        1.0
    );
    assert_eq!(
        features.as_array()[gp_routing::features::RELATIVE_COST_INDEX],
        0.25
    );
    assert_eq!(
        features.as_array()[gp_routing::features::COST_KNOWN_INDEX],
        1.0
    );

    let unknown_cost = FeatureVector::builder()
        .backend_index(31, 32)
        .relative_cost(None)
        .build();
    assert_eq!(
        unknown_cost.as_array()[gp_routing::features::RELATIVE_COST_INDEX],
        0.0
    );
    assert_eq!(
        unknown_cost.as_array()[gp_routing::features::COST_KNOWN_INDEX],
        0.0
    );
}

#[test]
fn test_ring_buffer_fifo() {
    let mut buffer = ObservationBuffer::new(5);
    for i in 0..10 {
        let features = FeatureVector::builder().dependency_count(i).build();
        buffer.push(features, i as f32 / 10.0);
    }

    assert_eq!(buffer.len(), 5);
    let (x, y) = buffer.as_training_data();
    assert_eq!(x.nrows(), 5);
    for row in 0..5 {
        let expected = (row + 5) as f32 / 10.0;
        assert_abs_diff_eq!(x[[row, DEP_COUNT_INDEX]], expected, epsilon = 1e-6);
        assert_abs_diff_eq!(y[row], expected, epsilon = 1e-6);
    }
}

#[test]
fn test_ring_buffer_training_data_shape() {
    let mut buffer = ObservationBuffer::new(64);
    for i in 0..50 {
        buffer.push(make_features(i % 2, i), (i % 10) as f32 / 10.0);
    }

    let (x, y) = buffer.as_training_data();
    assert_eq!(x.dim(), (50, FEATURE_DIM));
    assert_eq!(y.len(), 50);
}

#[test]
fn test_surrogate_unfitted_prior() {
    let config = GpRoutingConfig::builder()
        .prior_mean(0.42)
        .prior_variance(0.19)
        .build();
    let surrogate = GpSurrogate::new(config);
    let features = make_features(0, 0);
    let (mean, variance) = surrogate.predict(&features);

    assert_abs_diff_eq!(mean, 0.42, epsilon = 1e-6);
    assert_abs_diff_eq!(variance, 0.19, epsilon = 1e-6);
}

#[test]
fn test_surrogate_fit_and_predict() {
    let surrogate = train_two_backend_surrogate();
    assert!(surrogate.is_fitted());

    let backend0 = make_features(0, 200);
    let backend1 = make_features(1, 201);
    let (mean0, variance0) = surrogate.predict(&backend0);
    let (mean1, variance1) = surrogate.predict(&backend1);

    assert!(mean0 > mean1, "backend 0 should score above backend 1");
    assert!(variance0 >= 1e-6);
    assert!(variance1 >= 1e-6);
}

#[test]
fn test_rank_backends_ordering() {
    let surrogate = train_two_backend_surrogate();
    let base_features = make_features(0, 500);
    let ranked = rank_backends(&surrogate, &base_features, 2, AcquisitionStrategy::Greedy);

    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].0, 0);
    assert!(ranked[0].1 >= ranked[1].1);
}

#[test]
fn test_requests_since_fit_counter() {
    let mut buffer = ObservationBuffer::new(8);
    assert_eq!(buffer.requests_since_last_fit(), 0);

    buffer.push(make_features(0, 0), 0.8);
    buffer.push(make_features(1, 1), 0.2);
    assert_eq!(buffer.requests_since_last_fit(), 2);

    buffer.mark_fitted();
    assert_eq!(buffer.requests_since_last_fit(), 0);
}
