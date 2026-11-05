#[path = "common/mod.rs"]
mod support;

use criterion::{criterion_group, criterion_main, Criterion};
use iris_thumb_preview::jpeg;

fn bench_jpeg_thumb_extract(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("photo.jpg");
    std::fs::write(&path, support::encoded_test_jpeg(640, 480)).expect("write jpeg");
    c.bench_function("jpeg_thumb_extract/fit_or_resize", |b| {
        b.iter(|| {
            let result = jpeg::extract_jpeg_thumbnail_from_path(&path, 256).expect("thumbnail");
            std::hint::black_box(result);
        });
    });
}

criterion_group!(benches, bench_jpeg_thumb_extract);
criterion_main!(benches);
