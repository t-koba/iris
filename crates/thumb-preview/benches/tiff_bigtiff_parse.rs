#[path = "common/mod.rs"]
mod support;

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_tiff_bigtiff_parse(c: &mut Criterion) {
    let jpeg = support::encoded_test_jpeg(320, 200);
    let tiff = support::structured_tiff_with_embedded_jpeg(96, &jpeg);
    let bigtiff = support::bigtiff_with_embedded_jpeg(96, &jpeg);
    let tiff_path = support::unique_temp_path("dng");
    let bigtiff_path = support::unique_temp_path("nef");
    support::write_fixture(&tiff_path, &tiff);
    support::write_fixture(&bigtiff_path, &bigtiff);

    let mut group = c.benchmark_group("tiff_bigtiff_parse");
    group.bench_function("tiff", |b| {
        b.iter(|| support::bench_extract_raw_thumbnail(&tiff_path, 256));
    });
    group.bench_function("bigtiff", |b| {
        b.iter(|| support::bench_extract_raw_thumbnail(&bigtiff_path, 256));
    });
    group.finish();

    let _ = std::fs::remove_file(tiff_path);
    let _ = std::fs::remove_file(bigtiff_path);
}

criterion_group!(benches, bench_tiff_bigtiff_parse);
criterion_main!(benches);
