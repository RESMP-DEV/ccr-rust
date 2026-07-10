// Modified for CCR-Rust: benchmark the continuous relative-cost feature.
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gp_routing::{FeatureVector, GpRoutingConfig, GpSurrogate, ObservationBuffer};
use rand::Rng;

fn random_feature(rng: &mut impl Rng, backend_idx: usize, n_backends: usize) -> FeatureVector {
    FeatureVector::builder()
        .prompt_length(rng.gen_range(32..16_384))
        .priority(rng.gen_range(0..4) as u8)
        .has_verify(rng.gen_bool(0.5))
        .dependency_count(rng.gen_range(0..12))
        .retry_count(rng.gen_range(0..20))
        .backend_index(backend_idx, n_backends)
        .idle_ratio(rng.gen_range(0..8), 8)
        .in_flight_ratio(rng.gen_range(0..8), 8)
        .hour_of_day(rng.gen_range(0..24) as u32)
        .loop_count(rng.gen_range(0..4))
        .relative_cost(Some(rng.gen_range(0.0..=1.0)))
        .build()
}

fn build_buffer(n: usize, n_backends: usize) -> ObservationBuffer {
    let mut rng = rand::thread_rng();
    let mut buffer = ObservationBuffer::new(n.max(1));
    for sample in 0..n {
        let backend = sample % n_backends.max(1);
        let feature = random_feature(&mut rng, backend, n_backends);
        let outcome = rng.gen::<f32>();
        buffer.push(feature, outcome);
    }
    buffer
}

fn fit_surrogate(n: usize) -> GpSurrogate {
    let config = GpRoutingConfig::builder()
        .buffer_capacity(n.max(32))
        .min_observations(10)
        .n_backends(4)
        .build();
    let surrogate = GpSurrogate::new(config);
    let buffer = build_buffer(n, 4);
    surrogate.fit(&buffer).expect("benchmark surrogate fit");
    surrogate
}

fn bench_predict_single(c: &mut Criterion) {
    let surrogate = fit_surrogate(200);
    let mut rng = rand::thread_rng();
    let feature = random_feature(&mut rng, 1, 4);

    c.bench_function("bench_predict_single", |b| {
        b.iter(|| surrogate.predict(black_box(&feature)))
    });
}

fn bench_predict_batch_10(c: &mut Criterion) {
    let surrogate = fit_surrogate(200);
    let mut rng = rand::thread_rng();
    let batch: Vec<_> = (0..10)
        .map(|idx| random_feature(&mut rng, idx % 4, 4))
        .collect();

    c.bench_function("bench_predict_batch_10", |b| {
        b.iter(|| surrogate.predict_batch(black_box(&batch)))
    });
}

fn bench_fit_100(c: &mut Criterion) {
    c.bench_function("bench_fit_100", |b| {
        b.iter(|| {
            let config = GpRoutingConfig::builder()
                .buffer_capacity(100)
                .min_observations(10)
                .n_backends(4)
                .build();
            let surrogate = GpSurrogate::new(config);
            let buffer = build_buffer(100, 4);
            surrogate.fit(black_box(&buffer)).expect("fit 100");
        })
    });
}

fn bench_fit_500(c: &mut Criterion) {
    c.bench_function("bench_fit_500", |b| {
        b.iter(|| {
            let config = GpRoutingConfig::builder()
                .buffer_capacity(500)
                .min_observations(10)
                .n_backends(4)
                .build();
            let surrogate = GpSurrogate::new(config);
            let buffer = build_buffer(500, 4);
            surrogate.fit(black_box(&buffer)).expect("fit 500");
        })
    });
}

fn bench_feature_build(c: &mut Criterion) {
    let mut rng = rand::thread_rng();
    c.bench_function("bench_feature_build", |b| {
        b.iter(|| {
            let backend = rng.gen_range(0..4);
            random_feature(black_box(&mut rng), backend, 4)
        })
    });
}

criterion_group!(
    benches,
    bench_predict_single,
    bench_predict_batch_10,
    bench_fit_100,
    bench_fit_500,
    bench_feature_build
);
criterion_main!(benches);
