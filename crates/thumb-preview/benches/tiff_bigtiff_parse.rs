use iris_thumb_preview::test_fixtures as fixtures;

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_tiff_bigtiff_parse(c: &mut Criterion) {
    let jpeg = fixtures::jpeg(320, 200);
    let tiff = fixtures::tiff_with_jpeg(96, &jpeg);
    let bigtiff = fixtures::bigtiff_with_jpeg(96, &jpeg);
    let tiff_path = fixtures::temp_path("dng");
    let bigtiff_path = fixtures::temp_path("nef");
    std::fs::write(&tiff_path, &tiff).expect("write fixture");
    std::fs::write(&bigtiff_path, &bigtiff).expect("write fixture");

    let mut group = c.benchmark_group("tiff_bigtiff_parse");
    group.bench_function("tiff", |b| {
        b.iter(|| {
            std::hint::black_box(iris_thumb_preview::raw::extract_raw_thumbnail(
                &tiff_path,
                256,
                &tokio_util::sync::CancellationToken::new(),
            ))
        });
    });
    group.bench_function("bigtiff", |b| {
        b.iter(|| {
            std::hint::black_box(iris_thumb_preview::raw::extract_raw_thumbnail(
                &bigtiff_path,
                256,
                &tokio_util::sync::CancellationToken::new(),
            ))
        });
    });
    group.finish();

    let _ = std::fs::remove_file(tiff_path);
    let _ = std::fs::remove_file(bigtiff_path);
}

criterion_group!(benches, bench_tiff_bigtiff_parse);
criterion_main!(benches);
