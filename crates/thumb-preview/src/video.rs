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
            let rgb_frame = scale_to_rgb24(&frame, dst_w, dst_h)?;

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

            // Reserve a close estimate of the JPEG payload so encoding does not
            // repeatedly grow the buffer inside the timed extraction.
            let jpeg_capacity = (row_bytes * actual_h / 8).max(4096);
            let mut jpeg_buf = Cursor::new(Vec::with_capacity(jpeg_capacity));
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

// AVPixelFormat has the C int ABI on supported toolchains. Keep the decoded
// AVFrame.format as an integer: constructing a Rust enum for an unknown value
// would be undefined behavior, and the high-level Pixel conversion may panic.
#[cfg(feature = "video")]
const _: () = {
    assert!(std::mem::size_of::<std::ffi::c_int>() == 4);
    assert!(std::mem::size_of::<ffmpeg_next::ffi::AVPixelFormat>() == 4);
};

#[cfg(feature = "video")]
unsafe extern "C" {
    // These aliases preserve the C ABI while accepting raw pixel-format IDs.
    // Ask the linked library about support before context creation: some FFmpeg
    // versions assert in sws_getContext for unknown IDs rather than returning null.
    #[link_name = "sws_isSupportedInput"]
    fn sws_is_supported_input_raw(format: std::ffi::c_int) -> std::ffi::c_int;

    #[link_name = "sws_getContext"]
    fn sws_get_context_raw(
        src_w: std::ffi::c_int,
        src_h: std::ffi::c_int,
        src_format: std::ffi::c_int,
        dst_w: std::ffi::c_int,
        dst_h: std::ffi::c_int,
        dst_format: std::ffi::c_int,
        flags: std::ffi::c_int,
        src_filter: *mut ffmpeg_next::ffi::SwsFilter,
        dst_filter: *mut ffmpeg_next::ffi::SwsFilter,
        param: *const f64,
    ) -> *mut ffmpeg_next::ffi::SwsContext;
}

#[cfg(feature = "video")]
struct Scaler(std::ptr::NonNull<ffmpeg_next::ffi::SwsContext>);

#[cfg(feature = "video")]
impl Drop for Scaler {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns the context returned by sws_getContext.
        unsafe { ffmpeg_next::ffi::sws_freeContext(self.0.as_ptr()) };
    }
}

#[cfg(feature = "video")]
fn scale_to_rgb24(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
) -> Option<ffmpeg_next::frame::Video> {
    use ffmpeg_next::ffi;
    use std::{ffi::c_int, ptr};

    let dst_width = c_int::try_from(dst_w).ok()?;
    let dst_height = c_int::try_from(dst_h).ok()?;
    // SWS_BILINEAR from libswscale/swscale.h; ffmpeg-sys-next does not
    // expose this C macro consistently across target headers.
    const SWS_BILINEAR: c_int = 2;

    // SAFETY: frame owns a live AVFrame supplied by the decoder. Its source
    // format is read only as a C int, never converted to a Rust pixel enum.
    // The destination is allocated as known RGB24, and both frames remain alive
    // throughout the synchronous sws_scale call. FFmpeg's support query rejects
    // unknown/unsupported IDs; null and allocation/scale errors fail closed.
    unsafe {
        let src = &*frame.as_ptr();
        if src.width <= 0 || src.height <= 0 || dst_width <= 0 || dst_height <= 0 {
            return None;
        }
        if sws_is_supported_input_raw(src.format) <= 0 {
            return None;
        }
        let scaler = Scaler(ptr::NonNull::new(sws_get_context_raw(
            src.width,
            src.height,
            src.format,
            dst_width,
            dst_height,
            ffi::AVPixelFormat::AV_PIX_FMT_RGB24 as c_int,
            SWS_BILINEAR,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        ))?);

        let mut rgb = ffmpeg_next::frame::Video::empty();
        rgb.set_format(ffmpeg_next::format::Pixel::RGB24);
        rgb.set_width(dst_w);
        rgb.set_height(dst_h);
        if ffi::av_frame_get_buffer(rgb.as_mut_ptr(), 32) < 0 {
            return None;
        }
        let dst = &mut *rgb.as_mut_ptr();
        let rows = ffi::sws_scale(
            scaler.0.as_ptr(),
            src.data.as_ptr().cast(),
            src.linesize.as_ptr(),
            0,
            src.height,
            dst.data.as_ptr(),
            dst.linesize.as_ptr(),
        );
        (rows == dst_height).then_some(rgb)
    }
}

#[cfg(test)]
#[cfg(feature = "video")]
mod tests {
    use super::*;

    fn initialized_yuv420p() -> ffmpeg_next::frame::Video {
        let mut frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, 4, 4);
        // Limited-range YUV red, including padding in each allocated plane.
        frame.data_mut(0).fill(81);
        frame.data_mut(1).fill(90);
        frame.data_mut(2).fill(240);
        frame
    }

    #[test]
    fn scale_yuv420p_to_rgb24() {
        let frame = initialized_yuv420p();
        let rgb = scale_to_rgb24(&frame, 2, 2).expect("YUV420P should scale to RGB24");
        assert_eq!(rgb.format(), ffmpeg_next::format::Pixel::RGB24);
        assert_eq!((rgb.width(), rgb.height()), (2, 2));
        for y in 0..2 {
            let start = y * rgb.stride(0);
            for pixel in rgb.data(0)[start..start + 6].chunks_exact(3) {
                assert!(pixel[0] >= 252 && pixel[1] <= 2 && pixel[2] <= 2);
            }
        }
    }

    #[test]
    fn scale_unknown_pixel_format_returns_none() {
        let mut frame = initialized_yuv420p();
        // SAFETY: AVFrame.format is an integer, not a Rust enum. The owned
        // frame remains allocated, but its format ID is deliberately unknown.
        unsafe { (*frame.as_mut_ptr()).format = i32::MAX };
        assert!(scale_to_rgb24(&frame, 2, 2).is_none());
    }

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
