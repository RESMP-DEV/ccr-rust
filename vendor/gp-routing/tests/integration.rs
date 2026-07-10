// Modified for CCR-Rust: exercise the expanded backend and continuous-cost
// feature contract used by the embedded router.
use approx::assert_abs_diff_eq;
use gp_routing::features::{DEP_COUNT_INDEX, HOUR_COS_INDEX, HOUR_SIN_INDEX};
use gp_routing::{
    rank_backends, AcquisitionStrategy, FeatureVector, GpFitError, GpRoutingConfig, GpSurrogate,
    ObservationBuffer, FEATURE_DIM,
};
use std::time::{SystemTime, UNIX_EPOCH};

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
fn test_backend_counts_are_bounded_to_feature_capacity() {
    let config = GpRoutingConfig::builder().n_backends(usize::MAX).build();
    assert_eq!(config.n_backends, gp_routing::features::BACKEND_SLOTS);

    let surrogate = GpSurrogate::new(config);
    let ranked = rank_backends(
        &surrogate,
        &make_features(0, 0),
        usize::MAX,
        AcquisitionStrategy::Greedy,
    );
    assert_eq!(ranked.len(), gp_routing::features::BACKEND_SLOTS);
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
fn test_surrogate_rejects_invalid_kpls_dimensions_before_fitting() {
    for invalid in [0, FEATURE_DIM + 1] {
        let config = GpRoutingConfig::builder()
            .min_observations(1)
            .kpls_dim(Some(invalid))
            .build();
        let surrogate = GpSurrogate::new(config);
        let mut buffer = ObservationBuffer::new(2);
        buffer.push(make_features(0, 0), 0.5);

        assert!(matches!(
            surrogate.fit(&buffer),
            Err(GpFitError::InvalidConfig(_))
        ));
    }
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
fn test_batch_predictions_match_single_predictions() {
    let surrogate = train_two_backend_surrogate();
    let features = vec![make_features(0, 210), make_features(1, 211)];
    let batch = surrogate.predict_batch(&features);

    for (feature, (batch_mean, batch_variance)) in features.iter().zip(batch) {
        let (single_mean, single_variance) = surrogate.predict(feature);
        assert_abs_diff_eq!(batch_mean, single_mean, epsilon = 1e-5);
        assert_abs_diff_eq!(batch_variance, single_variance, epsilon = 1e-5);
    }
}

#[test]
fn test_fitted_surrogate_persistence_round_trip() {
    let surrogate = train_two_backend_surrogate();
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "gp-routing-test-{}-{suffix}.json",
        std::process::id()
    ));
    surrogate.save(&path).expect("save fitted surrogate");

    let loaded = GpSurrogate::new(
        GpRoutingConfig::builder()
            .buffer_capacity(128)
            .min_observations(20)
            .n_backends(2)
            .build(),
    );
    loaded.load(&path).expect("load fitted surrogate");
    std::fs::remove_file(&path).expect("remove persisted test model");

    for feature in [make_features(0, 220), make_features(1, 221)] {
        let before = surrogate.predict(&feature);
        let after = loaded.predict(&feature);
        assert_abs_diff_eq!(before.0, after.0, epsilon = 1e-5);
        assert_abs_diff_eq!(before.1, after.1, epsilon = 1e-5);
    }
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
fn test_thompson_ranking_is_complete_and_finite() {
    let surrogate = train_two_backend_surrogate();
    let ranked = rank_backends(
        &surrogate,
        &make_features(0, 510),
        2,
        AcquisitionStrategy::Thompson,
    );

    assert_eq!(ranked.len(), 2);
    let mut indices = ranked.iter().map(|(index, _)| *index).collect::<Vec<_>>();
    indices.sort_unstable();
    assert_eq!(indices, vec![0, 1]);
    assert!(ranked.iter().all(|(_, score)| score.is_finite()));
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
