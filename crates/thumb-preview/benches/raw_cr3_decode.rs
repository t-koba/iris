#[path = "common/mod.rs"]
mod support;

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_raw_cr3_decode(c: &mut Criterion) {
    let jpeg = support::encoded_test_jpeg(320, 200);
    let fixture = support::synthetic_cr3_with_second_sample_preview(&jpeg);
    let path = support::unique_temp_path("cr3");
    support::write_fixture(&path, &fixture);

    c.bench_function("raw_cr3_decode/extract_embedded_preview", |b| {
        b.iter(|| support::bench_extract_raw_thumbnail(&path, 256));
    });

    let _ = std::fs::remove_file(path);
}

criterion_group!(benches, bench_raw_cr3_decode);
criterion_main!(benches);
