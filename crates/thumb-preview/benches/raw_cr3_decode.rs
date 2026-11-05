use iris_thumb_preview::test_fixtures as fixtures;

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_raw_cr3_decode(c: &mut Criterion) {
    let jpeg = fixtures::jpeg(320, 200);
    let fixture = fixtures::cr3_with_jpeg(&jpeg, false);
    let path = fixtures::temp_path("cr3");
    std::fs::write(&path, &fixture).expect("write fixture");

    c.bench_function("raw_cr3_decode/extract_embedded_preview", |b| {
        b.iter(|| {
            std::hint::black_box(iris_thumb_preview::raw::extract_raw_thumbnail(
                &path,
                256,
                &tokio_util::sync::CancellationToken::new(),
            ))
        });
    });

    let _ = std::fs::remove_file(path);
}

criterion_group!(benches, bench_raw_cr3_decode);
criterion_main!(benches);
