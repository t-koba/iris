//! Fast integer-block thumbnail path for shrinking 8-bit RGB images.
//!
//! Upstream `DynamicImage::thumbnail` also performs fractional interpolation
//! when a window has an empty axis, which happens for equal-size and
//! upscaling fits. This module handles only strictly shrinking fits, where
//! every window contains full source pixels, and delegates every other input
//! to the upstream implementation unchanged.

use image::{DynamicImage, ImageBuffer, Rgb};

/// Downscale `image` to fit `max_dimension`, producing the same pixels as
/// `DynamicImage::thumbnail` while avoiding per-pixel generic dispatch for
/// decoded 8-bit RGB images.
pub(crate) fn thumbnail(image: &DynamicImage, max_dimension: u32) -> DynamicImage {
    match image {
        DynamicImage::ImageRgb8(rgb) => {
            let (width, height) = resize_dimensions(rgb.width(), rgb.height(), max_dimension);
            if width >= rgb.width() || height >= rgb.height() {
                // The block-only fast path requires both axes to shrink;
                // upstream handles equal-size and upscaling inputs exactly.
                image.thumbnail(max_dimension, max_dimension)
            } else {
                DynamicImage::ImageRgb8(thumbnail_rgb8(rgb, width, height))
            }
        }
        _ => image.thumbnail(max_dimension, max_dimension),
    }
}

/// Aspect-preserving fit used by `DynamicImage::thumbnail`.
fn resize_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    let wratio = f64::from(max_dimension) / f64::from(width);
    let hratio = f64::from(max_dimension) / f64::from(height);
    let ratio = wratio.min(hratio);
    let new_width = (f64::from(width) * ratio).round().max(1.0) as u64;
    let new_height = (f64::from(height) * ratio).round().max(1.0) as u64;
    if new_width > u64::from(u32::MAX) {
        let ratio = f64::from(u32::MAX) / f64::from(width);
        (
            u32::MAX,
            (f64::from(height) * ratio).round().max(1.0) as u32,
        )
    } else if new_height > u64::from(u32::MAX) {
        let ratio = f64::from(u32::MAX) / f64::from(height);
        ((f64::from(width) * ratio).round().max(1.0) as u32, u32::MAX)
    } else {
        (new_width as u32, new_height as u32)
    }
}

fn clamp_u8(value: u32) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

fn thumbnail_rgb8(
    source: &ImageBuffer<Rgb<u8>, Vec<u8>>,
    new_width: u32,
    new_height: u32,
) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let (width, height) = source.dimensions();
    let mut out = ImageBuffer::new(new_width, new_height);
    if height == 0 || width == 0 {
        return out;
    }

    let raw = source.as_raw();
    let x_ratio = width as f32 / new_width as f32;
    let y_ratio = height as f32 / new_height as f32;
    let pixel = |x: u32, y: u32| -> (u32, u32, u32) {
        let index = (y as usize * width as usize + x as usize) * 3;
        (
            u32::from(raw[index]),
            u32::from(raw[index + 1]),
            u32::from(raw[index + 2]),
        )
    };

    for outy in 0..new_height {
        let bottomf = outy as f32 * y_ratio;
        let topf = bottomf + y_ratio;
        let bottom = (bottomf.ceil() as u32).min(height - 1);
        let top = (topf.ceil() as u32).clamp(bottom, height);

        for outx in 0..new_width {
            let leftf = outx as f32 * x_ratio;
            let rightf = leftf + x_ratio;
            let left = (leftf.ceil() as u32).min(width - 1);
            let right = (rightf.ceil() as u32).clamp(left, width);

            // A shrinking fit makes both ratios > 1, so every window covers
            // at least one full source pixel on each axis and only the integer
            // block average is possible.
            debug_assert!(bottom < top && left < right);
            let mut sr = 0u32;
            let mut sg = 0u32;
            let mut sb = 0u32;
            for y in bottom..top {
                for x in left..right {
                    let (pr, pg, pb) = pixel(x, y);
                    sr += pr;
                    sg += pg;
                    sb += pb;
                }
            }
            let n = (right - left) * (top - bottom);
            let round = n / 2;
            let r = clamp_u8((sr + round) / n);
            let g = clamp_u8((sg + round) / n);
            let b = clamp_u8((sb + round) / n);
            out.put_pixel(outx, outy, [r, g, b].into());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb8_thumbnail_matches_upstream_pixels() {
        let source = ImageBuffer::from_fn(4000, 3000, |x, y| {
            Rgb([(x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8])
        });
        let dynamic = DynamicImage::ImageRgb8(source);
        for &bound in &[16u32, 64, 256, 500, 2000] {
            let expected = dynamic.thumbnail(bound, bound).to_rgb8();
            let actual = thumbnail(&dynamic, bound).to_rgb8();
            assert_eq!(actual.dimensions(), expected.dimensions());
            assert_eq!(actual.as_raw(), expected.as_raw());
        }
    }

    #[test]
    fn rgb8_thumbnail_matches_upstream_fraction_paths() {
        let source = ImageBuffer::from_fn(100, 80, |x, y| {
            Rgb([
                (x * 7 % 256) as u8,
                (y * 11 % 256) as u8,
                ((x + 3 * y) % 256) as u8,
            ])
        });
        let dynamic = DynamicImage::ImageRgb8(source);
        for &bound in &[64u32, 256] {
            let expected = dynamic.thumbnail(bound, bound).to_rgb8();
            let actual = thumbnail(&dynamic, bound).to_rgb8();
            assert_eq!(actual.dimensions(), expected.dimensions());
            assert_eq!(actual.as_raw(), expected.as_raw());
        }
    }

    #[test]
    fn upscaling_inputs_delegate_to_upstream() {
        // The reported 2x2 -> 23 case exercises upstream's both-axis
        // fractional interpolation, where f32 rounding is observable.
        let source = ImageBuffer::from_fn(2, 2, |x, y| {
            Rgb([(x * 13 + y * 7) as u8, (y * 29) as u8, (x * 3 + y) as u8])
        });
        let dynamic = DynamicImage::ImageRgb8(source);
        for &bound in &[23u32, 31, 47, 65, 127, 200] {
            let expected = dynamic.thumbnail(bound, bound).to_rgb8();
            let actual = thumbnail(&dynamic, bound).to_rgb8();
            assert_eq!(actual.dimensions(), expected.dimensions());
            assert_eq!(actual.as_raw(), expected.as_raw());
        }
    }

    #[test]
    fn non_rgb_images_keep_upstream_thumbnail() {
        let dynamic = DynamicImage::ImageRgba8(ImageBuffer::from_fn(80, 60, |x, y| {
            image::Rgba([x as u8, y as u8, 128, 200])
        }));
        let expected = dynamic.thumbnail(32, 32).to_rgba8();
        let actual = thumbnail(&dynamic, 32).to_rgba8();
        assert_eq!(actual.as_raw(), expected.as_raw());
    }
}
