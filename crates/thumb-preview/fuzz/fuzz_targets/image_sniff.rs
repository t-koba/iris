#![no_main]

use iris_core_domain::types::thumbnail::{ImageFormat, ThumbnailHint};
use iris_thumb_preview::image_generic::{
    extract_image_thumbnail_from_bytes, probe_passthrough_candidate,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 2 * 1024 * 1024 {
        return;
    }

    let hint = ThumbnailHint {
        max_dimension: 256,
        preferred_formats: vec![ImageFormat::Jpeg, ImageFormat::Png, ImageFormat::WebP],
    };
    let _ = probe_passthrough_candidate(data, &hint);
    let _ = extract_image_thumbnail_from_bytes(data, &hint);
});
