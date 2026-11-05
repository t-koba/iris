#[cfg(not(feature = "video"))]
use iris_core_domain::types::thumbnail::ThumbnailResult;
#[cfg(feature = "video")]
use iris_core_domain::types::thumbnail::{ImageFormat, ThumbnailResult, ThumbnailSource};
#[cfg(feature = "video")]
use std::io::{Cursor, Write};
use std::path::Path;
#[cfg(feature = "video")]
use std::sync::{Once, OnceLock};
#[cfg(feature = "video")]
use tracing::debug;

#[cfg(feature = "video")]
static FFMPEG_INIT: Once = Once::new();
#[cfg(feature = "video")]
static FFMPEG_INIT_OK: OnceLock<bool> = OnceLock::new();

#[cfg(feature = "video")]
fn ensure_ffmpeg_init() -> bool {
    FFMPEG_INIT.call_once(|| {
        let ok = ffmpeg_next::init().is_ok();
        let _ = FFMPEG_INIT_OK.set(ok);
    });

    FFMPEG_INIT_OK.get().copied().unwrap_or(false)
}

/// Largest size within `max_dim` that keeps the aspect ratio.
#[cfg(feature = "video")]
fn fit_dimensions(src_w: u32, src_h: u32, max_dim: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (max_dim, max_dim);
    }
    if src_w <= max_dim && src_h <= max_dim {
        return (src_w, src_h);
    }
    let scale = (max_dim as f64 / src_w as f64).min(max_dim as f64 / src_h as f64);
    let dst_w = ((src_w as f64 * scale).round() as u32).max(1);
    let dst_h = ((src_h as f64 * scale).round() as u32).max(1);
    (dst_w, dst_h)
}

/// Uses the first keyframe.
#[cfg(feature = "video")]
pub fn extract_video_thumbnail(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
    if !ensure_ffmpeg_init() {
        return None;
    }
    extract_keyframe(file_path, max_dimension)
}

/// FFmpeg's mature still-image codecs are a fallback for variants which a
/// format-specific decoder cannot represent (notably HDR JPEG XR and uncommon HEIF).
#[cfg(feature = "video")]
pub fn extract_still_thumbnail(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
    if !ensure_ffmpeg_init() {
        return None;
    }
    extract_keyframe(file_path, max_dimension)
}

#[cfg(feature = "video")]
pub fn extract_still_thumbnail_from_bytes(
    data: &[u8],
    extension: &str,
    max_dimension: u32,
) -> Option<ThumbnailResult> {
    let suffix = format!(".{extension}");
    let mut file = tempfile::Builder::new()
        .prefix("iris-image-decode-")
        .suffix(&suffix)
        .tempfile()
        .ok()?;
    file.write_all(data).ok()?;
    file.flush().ok()?;
    extract_still_thumbnail(file.path(), max_dimension)
}

#[cfg(feature = "video")]
pub const fn supports_video_thumbnails() -> bool {
    true
}

#[cfg(not(feature = "video"))]
pub fn extract_still_thumbnail(_file_path: &Path, _max_dimension: u32) -> Option<ThumbnailResult> {
    None
}

#[cfg(not(feature = "video"))]
pub fn extract_still_thumbnail_from_bytes(
    _data: &[u8],
    _extension: &str,
    _max_dimension: u32,
) -> Option<ThumbnailResult> {
    None
}

#[cfg(not(feature = "video"))]
pub fn extract_video_thumbnail(_file_path: &Path, _max_dimension: u32) -> Option<ThumbnailResult> {
    None
}

#[cfg(not(feature = "video"))]
pub const fn supports_video_thumbnails() -> bool {
    false
}

#[cfg(feature = "video")]
fn extract_keyframe(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
    let mut ictx = ffmpeg_next::format::input(&file_path).ok()?;

    let video_stream_index = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)?
        .index();

    let codec_params = ictx.stream(video_stream_index)?.parameters();

    let mut decoder = ffmpeg_next::codec::context::Context::from_parameters(codec_params)
        .ok()?
        .decoder()
        .video()
        .ok()?;

    let mut frame = ffmpeg_next::frame::Video::empty();

    for (stream, packet) in ictx.packets() {
        if stream.index() != video_stream_index {
            continue;
        }

        decoder.send_packet(&packet).ok()?;

        if decoder.receive_frame(&mut frame).is_ok() {
            let src_w = frame.width();
            let src_h = frame.height();

            debug!(
                "extracted keyframe {}x{} from {:?}",
                src_w, src_h, file_path
            );

            let (dst_w, dst_h) = fit_dimensions(src_w, src_h, max_dimension);

            // Resize and convert the pixel format in one swscale pass.
            let mut scaler = ffmpeg_next::software::scaling::Context::get(
                frame.format(),
                src_w,
                src_h,
                ffmpeg_next::format::Pixel::RGB24,
                dst_w,
                dst_h,
                ffmpeg_next::software::scaling::Flags::BILINEAR,
            )
            .ok()?;

            let mut rgb_frame = ffmpeg_next::frame::Video::empty();
            scaler.run(&frame, &mut rgb_frame).ok()?;

            // Repack rows when the stride includes padding.
            let stride = rgb_frame.stride(0);
            let actual_w = rgb_frame.width() as usize;
            let actual_h = rgb_frame.height() as usize;
            let row_bytes = actual_w * 3; // RGB24 = 3 bytes per pixel
            let plane = rgb_frame.data(0);

            let pixels = if stride == row_bytes {
                plane[..row_bytes * actual_h].to_vec()
            } else {
                let mut buf = Vec::with_capacity(row_bytes * actual_h);
                for y in 0..actual_h {
                    let start = y * stride;
                    buf.extend_from_slice(&plane[start..start + row_bytes]);
                }
                buf
            };

            let img = image::RgbImage::from_raw(actual_w as u32, actual_h as u32, pixels)?;

            let mut jpeg_buf = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(img)
                .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
                .ok()?;

            return Some(ThumbnailResult {
                data: jpeg_buf.into_inner(),
                format: ImageFormat::Jpeg,
                width: actual_w as u32,
                height: actual_h as u32,
                source: ThumbnailSource::PartialDecode,
            });
        }
    }

    None
}

#[cfg(test)]
#[cfg(feature = "video")]
mod tests {
    use super::*;

    #[test]
    fn test_fit_dimensions_landscape() {
        assert_eq!(fit_dimensions(1920, 1080, 256), (256, 144));
    }

    #[test]
    fn test_fit_dimensions_portrait() {
        assert_eq!(fit_dimensions(1080, 1920, 256), (144, 256));
    }

    #[test]
    fn test_fit_dimensions_square() {
        assert_eq!(fit_dimensions(1000, 1000, 256), (256, 256));
    }

    #[test]
    fn test_fit_dimensions_already_small() {
        assert_eq!(fit_dimensions(100, 50, 256), (100, 50));
    }
}
