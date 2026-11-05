use criterion::{criterion_group, criterion_main, Criterion};
use iris_search::bloom::{BloomQuery, BloomSignature};
use iris_search::segment::{IndexedKind, Segment, SegmentRecord};

fn make_segment(count: usize) -> Segment {
    Segment::new(
        (0..count)
            .map(|index| SegmentRecord {
                path: format!("/photos/2024/{index:06}.jpg"),
                name: format!("{index:06}.jpg"),
                size: 1_000_000 + index as u64,
                mtime_secs: 1_700_000_000 + index as i64,
                kind: IndexedKind::Image,
                width: Some(4000),
                height: Some(3000),
                capture_secs: Some(1_700_000_000 + index as i64),
                capture_offset_min: Some(540),
                camera: Some(format!("Camera {}", index % 8)),
                lens: Some(format!("Lens {}", index % 16)),
                gps_lat: Some(35.0 + (index % 100) as f32 / 1000.0),
                gps_lon: Some(139.0 + (index % 100) as f32 / 1000.0),
                content_signature: (index % 10 == 0)
                    .then(|| BloomSignature::from_content("日本語 PDF metadata")),
            })
            .collect(),
    )
}

fn bench_segment_scan(c: &mut Criterion) {
    let segment = make_segment(100_000);
    c.bench_function("segment_scan_100k/camera_date_gps", |b| {
        b.iter(|| {
            let hits = segment.scan(|record| {
                record.camera.as_deref() == Some("Camera 3")
                    && record
                        .capture_secs
                        .is_some_and(|secs| secs >= 1_700_050_000)
                    && record.gps_lat.is_some_and(|lat| lat > 35.05)
            });
            std::hint::black_box(hits.len())
        })
    });
}

fn bench_bloom_query(c: &mut Criterion) {
    let signatures = (0..10_000)
        .map(|index| BloomSignature::from_content(&format!("日本語 PDF metadata {index}")))
        .collect::<Vec<_>>();
    let query = BloomQuery::new("日本語");
    c.bench_function("bloom_query/10k_japanese", |b| {
        b.iter(|| {
            let hits = signatures
                .iter()
                .filter(|signature| signature.might_contain(&query))
                .count();
            std::hint::black_box(hits)
        })
    });
}

criterion_group!(benches, bench_segment_scan, bench_bloom_query);
criterion_main!(benches);
