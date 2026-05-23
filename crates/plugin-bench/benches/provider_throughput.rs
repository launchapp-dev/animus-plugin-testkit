use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

fn scenario_loop_overhead(c: &mut Criterion) {
    c.bench_function("scenario_yaml_parse", |b| {
        let yaml = include_str!("../../../scenarios/streaming-short.yaml");
        b.iter(|| {
            let _: testkit_core::ScenarioFile = serde_yaml::from_str(yaml).unwrap();
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(3));
    targets = scenario_loop_overhead
}
criterion_main!(benches);
