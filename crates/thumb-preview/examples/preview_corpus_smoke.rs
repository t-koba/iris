//! Correctness smoke for the pinned corpus; timings are diagnostics, not parity evidence.

use iris_core_domain::types::thumbnail::{
    ImageFormat, ThumbnailHint, ThumbnailResult, ThumbnailSource,
};
use iris_thumb_preview::{encoded, image_generic, pdf, raw, video};
use std::{error::Error, path::Path, time::Instant};
use tokio_util::sync::CancellationToken;

const MAX_DIMENSION: u32 = 256;

fn check(
    name: &str,
    result: Option<ThumbnailResult>,
    start: Instant,
    require_nonuniform_pixels: bool,
) -> ThumbnailResult {
    let thumb = result.unwrap_or_else(|| panic!("{name}: preview extraction failed"));
    assert_eq!(thumb.format, ImageFormat::Jpeg, "{name}: output format");
    let decoded = image::load_from_memory_with_format(&thumb.data, image::ImageFormat::Jpeg)
        .unwrap_or_else(|err| panic!("{name}: invalid JPEG: {err}"));
    assert_eq!(
        (decoded.width(), decoded.height()),
        (thumb.width, thumb.height)
    );
    assert!(thumb.width > 0 && thumb.height > 0, "{name}: empty output");
    assert!(thumb.width <= MAX_DIMENSION && thumb.height <= MAX_DIMENSION);
    // Reject blank output as well as header-only or dimension-only successes.
    if require_nonuniform_pixels {
        let pixels = decoded.to_rgb8();
        let first = pixels.get_pixel(0, 0);
        assert!(
            pixels.pixels().any(|pixel| pixel != first),
            "{name}: blank output"
        );
    }
    println!(
        "{name}: {}x{}, {} bytes, {:?}, diagnostic elapsed {:?}",
        thumb.width,
        thumb.height,
        thumb.data.len(),
        thumb.source,
        start.elapsed()
    );
    thumb
}

fn main() -> Result<(), Box<dyn Error>> {
    let dir = std::env::args_os()
        .nth(1)
        .ok_or("expected corpus directory")?;
    let dir = Path::new(&dir);
    assert!(video::supports_video_thumbnails());
    assert!(pdf::supports_pdf_thumbnails());
    // CR3 alone is also supported by raw-core; check full RAW feature availability explicitly.
    assert!(raw::supports_raw_extension("raf") && raw::supports_raw_extension("x3f"));
    let hint = ThumbnailHint {
        max_dimension: MAX_DIMENSION,
        preferred_formats: vec![ImageFormat::Jpeg],
    };

    let jpeg = dir.join("landscape.jpg");
    let info = encoded::probe_encoded_image_path(&jpeg).ok_or("missing JPEG dimensions")?;
    assert_eq!((info.width, info.height), (4000, 3000));
    let start = Instant::now();
    let thumb = check(
        "high-res JPEG",
        image_generic::extract_image_thumbnail(&jpeg, &hint),
        start,
        true,
    );
    assert_eq!((thumb.width, thumb.height), (256, 192));

    let small_jpeg = dir.join("sample.jpg");
    let start = Instant::now();
    let thumb = check(
        "small JPEG passthrough",
        image_generic::extract_image_thumbnail(&small_jpeg, &hint),
        start,
        false,
    );
    assert_eq!((thumb.width, thumb.height), (200, 200));
    assert_eq!(thumb.source, ThumbnailSource::OriginalEncoded);
    assert_eq!(thumb.data, std::fs::read(&small_jpeg)?);

    let start = Instant::now();
    let thumb = check(
        "CR3",
        raw::extract_raw_thumbnail(
            &dir.join("canon-eos-r.cr3"),
            MAX_DIMENSION,
            &CancellationToken::new(),
        ),
        start,
        true,
    );
    assert_eq!(thumb.source, ThumbnailSource::EmbeddedPreview);

    let start = Instant::now();
    let thumb = check(
        "H.264 MP4",
        video::extract_video_thumbnail(&dir.join("bunny.mp4"), MAX_DIMENSION),
        start,
        false, // The pinned clip's first keyframe is an intentional black fade-in.
    );
    assert_eq!((thumb.width, thumb.height), (256, 144));
    assert_eq!(thumb.source, ThumbnailSource::PartialDecode);

    let path = dir.join("one-page.pdf");
    assert_eq!(pdf::pdf_page_count(&path), Some(1));
    let start = Instant::now();
    check(
        "PDF",
        pdf::extract_pdf_thumbnail(&path, MAX_DIMENSION),
        start,
        true,
    );
    println!("Corpus correctness smoke passed; latency/format parity remains unmeasured.");
    Ok(())
}
