use iris_search::bloom::BloomSignature;
use iris_search::filter::{
    facets_for_records, filter_segment, verify_content_match, FilterModel, GpsBounds, GpsFilter,
    RangeFilter, Scope,
};
use iris_search::segment::{IndexedKind, Segment, SegmentRecord};
use iris_search::stream::{filter_segment_incremental, FilterEvent, FilterStats};
use iris_search::token::{format_filter_tokens, parse_filter_tokens};
use iris_search::walker::{walk_local, WalkOptions};
use std::fs;
use std::time::Instant;

const RECORDS: usize = 100_000;
const PDF_SIGNATURES: usize = 10_000;
const MAX_SEGMENT_BYTES: usize = 30 * 1024 * 1024;
const MAX_SCAN_MS: u128 = 250;

fn main() {
    let segment = make_segment();
    let bytes = segment.to_bytes().expect("encode segment");
    assert!(
        bytes.len() <= MAX_SEGMENT_BYTES,
        "segment is too large: {} bytes > {} bytes",
        bytes.len(),
        MAX_SEGMENT_BYTES
    );
    let restored = Segment::from_bytes(&bytes).expect("decode segment");
    assert_eq!(restored.records().len(), RECORDS);

    let model = FilterModel {
        text: String::new(),
        scope: Scope::Recursive,
        kinds: vec![IndexedKind::Image],
        date: Some(RangeFilter {
            min: Some(1_704_067_200),
            max: Some(1_720_051_199),
        }),
        size: None,
        camera: None,
        lens: Some("Lens 7".to_string()),
        gps: GpsFilter::Bounds(GpsBounds {
            min_lat: 35.035,
            max_lat: 35.070,
            min_lon: 139.035,
            max_lon: 139.070,
        }),
        content: false,
    };

    let start = Instant::now();
    let hits = filter_segment(&restored, model);
    let elapsed = start.elapsed();
    assert!(
        !hits.is_empty(),
        "photo date/lens/GPS validation filter returned no hits"
    );
    assert!(
        elapsed.as_millis() <= MAX_SCAN_MS,
        "warm filter scan too slow: {} ms > {} ms",
        elapsed.as_millis(),
        MAX_SCAN_MS
    );
    validate_facets(&restored);
    validate_token_round_trip();
    let incremental = validate_incremental_stream(&restored);

    let pdf_hits = filter_segment(
        &restored,
        FilterModel {
            text: "日本語語句".to_string(),
            scope: Scope::Recursive,
            kinds: vec![IndexedKind::Pdf],
            date: None,
            size: None,
            camera: None,
            lens: None,
            gps: GpsFilter::Any,
            content: true,
        },
    );
    assert_eq!(
        pdf_hits.len(),
        PDF_SIGNATURES,
        "PDF bloom candidate count changed"
    );

    let snippet = verify_content_match("前文 日本語語句を含むPDF本文 後文", "日本語語句", 6)
        .expect("snippet");
    assert!(
        snippet.snippet.contains("日本語語句"),
        "snippet does not contain the verified query: {}",
        snippet.snippet
    );
    let walked = validate_local_walker();

    println!("records={RECORDS}");
    println!("pdf_signatures={PDF_SIGNATURES}");
    println!("segment_bytes={}", bytes.len());
    println!("photo_filter_hits={}", hits.len());
    println!("photo_filter_ms={}", elapsed.as_millis());
    println!("incremental_scanned={}", incremental.scanned);
    println!("incremental_matched={}", incremental.matched);
    println!("walker_records={walked}");
    println!("snippet={}", snippet.snippet);
}

fn validate_facets(segment: &Segment) {
    let facets = facets_for_records(segment.records());
    assert_eq!(
        facets.kinds.get(&IndexedKind::Pdf).copied(),
        Some(PDF_SIGNATURES)
    );
    assert_eq!(
        facets.kinds.get(&IndexedKind::Image).copied(),
        Some(RECORDS - PDF_SIGNATURES)
    );
    assert_eq!(facets.cameras.len(), 8);
    assert_eq!(facets.lenses.len(), 16);
    assert_eq!(facets.gps_present, RECORDS - PDF_SIGNATURES);
    assert_eq!(facets.gps_absent, PDF_SIGNATURES);
    assert_eq!(facets.gps_points.len(), RECORDS - PDF_SIGNATURES);
}

fn validate_token_round_trip() {
    let model = FilterModel {
        text: "日本語語句".to_string(),
        scope: Scope::Recursive,
        kinds: vec![IndexedKind::Image, IndexedKind::Pdf],
        date: Some(RangeFilter {
            min: Some(1_704_067_200),
            max: Some(1_720_051_199),
        }),
        size: Some(RangeFilter {
            min: Some(1_000_000),
            max: Some(9_000_000),
        }),
        camera: Some("Camera 1".to_string()),
        lens: Some("Lens 7".to_string()),
        gps: GpsFilter::Bounds(GpsBounds {
            min_lat: 35.035,
            max_lat: 35.070,
            min_lon: 139.035,
            max_lon: 139.070,
        }),
        content: true,
    };
    let tokens = format_filter_tokens(&model);
    let parsed = parse_filter_tokens(&tokens);
    assert!(
        parsed.warnings.is_empty(),
        "formatted validation tokens should parse without warnings: {:?}",
        parsed.warnings
    );
    assert_eq!(parsed.model, model);
}

fn validate_incremental_stream(segment: &Segment) -> FilterStats {
    let events = filter_segment_incremental(
        segment,
        FilterModel {
            text: String::new(),
            scope: Scope::Recursive,
            kinds: vec![IndexedKind::Pdf],
            date: None,
            size: None,
            camera: None,
            lens: None,
            gps: GpsFilter::Any,
            content: false,
        },
        777,
        || false,
    );
    assert!(matches!(
        events.first(),
        Some(FilterEvent::Started {
            total_hint: Some(RECORDS)
        })
    ));
    let hit_batches = events
        .iter()
        .filter(|event| matches!(event, FilterEvent::Hits(_)))
        .count();
    assert!(
        hit_batches > 1,
        "incremental validation should emit multiple hit batches"
    );
    match events.last() {
        Some(FilterEvent::Finished(stats)) => {
            assert_eq!(stats.scanned, RECORDS);
            assert_eq!(stats.matched, PDF_SIGNATURES);
            stats.clone()
        }
        other => panic!("incremental validation did not finish: {other:?}"),
    }
}

fn validate_local_walker() -> usize {
    let dir = tempfile::tempdir().expect("validation tempdir");
    fs::create_dir(dir.path().join("nested")).expect("nested dir");
    fs::write(dir.path().join("photo.jpg"), b"image").expect("photo");
    fs::write(dir.path().join("document.pdf"), b"pdf").expect("pdf");
    fs::write(dir.path().join("nested/movie.mp4"), b"video").expect("video");
    fs::write(dir.path().join(".hidden.txt"), b"hidden").expect("hidden");

    let recursive = walk_local(dir.path(), &WalkOptions::default()).expect("recursive walk");
    let names = recursive
        .records
        .iter()
        .map(|record| (record.name.as_str(), record.kind))
        .collect::<Vec<_>>();
    assert!(names.contains(&("photo.jpg", IndexedKind::Image)));
    assert!(names.contains(&("document.pdf", IndexedKind::Pdf)));
    assert!(names.contains(&("movie.mp4", IndexedKind::Video)));
    assert!(names.contains(&("nested", IndexedKind::Directory)));
    assert!(!names.iter().any(|(name, _)| *name == ".hidden.txt"));
    assert_eq!(recursive.stats.visited_dirs, 2);
    assert_eq!(recursive.stats.skipped_hidden, 1);

    let non_recursive = walk_local(
        dir.path(),
        &WalkOptions {
            recursive: false,
            include_hidden: false,
            max_entries: None,
        },
    )
    .expect("non-recursive walk");
    assert!(!non_recursive
        .records
        .iter()
        .any(|record| record.name == "movie.mp4"));

    recursive.records.len()
}

fn make_segment() -> Segment {
    Segment::new((0..RECORDS).map(make_record).collect())
}

fn make_record(index: usize) -> SegmentRecord {
    let is_pdf = index < PDF_SIGNATURES;
    let camera_index = index % 8;
    let lens_index = index % 16;
    let gps_bucket = index % 100;
    SegmentRecord {
        path: if is_pdf {
            format!("/validation/pdfs/report-{index:05}.pdf")
        } else {
            format!("/validation/photos/2024/img-{index:06}.jpg")
        },
        name: if is_pdf {
            format!("report-{index:05}.pdf")
        } else {
            format!("img-{index:06}.jpg")
        },
        size: if is_pdf {
            750_000 + index as u64
        } else {
            4_000_000 + index as u64
        },
        mtime_secs: 1_704_067_200 + index as i64,
        kind: if is_pdf {
            IndexedKind::Pdf
        } else {
            IndexedKind::Image
        },
        width: (!is_pdf).then_some(4000),
        height: (!is_pdf).then_some(3000),
        capture_secs: (!is_pdf).then_some(1_704_067_200 + index as i64),
        capture_offset_min: (!is_pdf).then_some(540),
        camera: (!is_pdf).then(|| format!("Camera {camera_index}")),
        lens: (!is_pdf).then(|| format!("Lens {lens_index}")),
        gps_lat: (!is_pdf).then_some(35.0 + gps_bucket as f32 / 1000.0),
        gps_lon: (!is_pdf).then_some(139.0 + gps_bucket as f32 / 1000.0),
        content_signature: is_pdf.then(|| BloomSignature::from_content("日本語語句 PDF snippet")),
    }
}
