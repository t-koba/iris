use crate::encoded::probe_encoded_image_path;
use iris_core_domain::types::thumbnail::{ImageFormat, ThumbnailResult, ThumbnailSource};
use jpeg_decoder::{Decoder as JpegFastDecoder, PixelFormat};
use std::fs::File;
use std::io::BufReader;
use std::io::Cursor;
use std::io::Read;
use std::path::Path;
use tracing::debug;

const JPEG_EXIF_SCAN_BYTES: usize = 96 * 1024;

/// Tries the embedded thumbnail from the file head first and decodes the whole file only if that fails.
pub fn extract_jpeg_thumbnail_from_path(
    file_path: &Path,
    max_dimension: u32,
) -> Option<ThumbnailResult> {
    if let Some(head) = read_head(file_path, JPEG_EXIF_SCAN_BYTES) {
        if let Some(result) = extract_embedded_jpeg_thumbnail(&head, max_dimension) {
            debug!("extracted EXIF thumbnail from head scan");
            return Some(result);
        }
    }

    if let Some(info) = probe_encoded_image_path(file_path) {
        if info.format == ImageFormat::Jpeg
            && info.width <= max_dimension
            && info.height <= max_dimension
        {
            let bytes = std::fs::read(file_path).ok()?;
            return Some(existing_jpeg_thumbnail(
                bytes,
                info.width,
                info.height,
                ThumbnailSource::OriginalEncoded,
            ));
        }
    }

    decode_and_resize_from_path_fast_jpeg(file_path, max_dimension)
}

pub fn extract_embedded_jpeg_thumbnail(data: &[u8], max_dimension: u32) -> Option<ThumbnailResult> {
    if !has_exif_app1(data) {
        return None;
    }

    extract_exif_thumbnail(data, max_dimension)
}

/// Marker segments before the scan data, as `(marker, payload after the length field)`.
fn jpeg_segments(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut i = if data.starts_with(&[0xFF, 0xD8]) {
        2
    } else {
        data.len()
    };
    std::iter::from_fn(move || {
        while i + 1 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            while i < data.len() && data[i] == 0xFF {
                i += 1;
            }
            let marker = *data.get(i)?;
            i += 1;
            // Standalone markers carry no length.
            if matches!(marker, 0x01 | 0xD0..=0xD9) {
                continue;
            }
            if marker == 0xDA || i + 1 >= data.len() {
                return None;
            }
            let segment_len = usize::from(u16::from_be_bytes([data[i], data[i + 1]]));
            if segment_len < 2 || i + segment_len > data.len() {
                return None;
            }
            let payload = &data[i + 2..i + segment_len];
            i += segment_len;
            return Some((marker, payload));
        }
        None
    })
}

fn has_exif_app1(data: &[u8]) -> bool {
    jpeg_segments(data).any(|(marker, payload)| marker == 0xE1 && payload.starts_with(b"Exif\0\0"))
}

/// Reads the dimensions from the SOF marker.
pub fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    jpeg_segments(data).find_map(|(marker, payload)| {
        if !is_sof_marker(marker) || payload.len() < 5 {
            return None;
        }
        let height = u32::from(u16::from_be_bytes([payload[1], payload[2]]));
        let width = u32::from(u16::from_be_bytes([payload[3], payload[4]]));
        (width > 0 && height > 0).then_some((width, height))
    })
}

fn extract_exif_thumbnail(data: &[u8], max_dimension: u32) -> Option<ThumbnailResult> {
    let reader = exif::Reader::new();
    let exif_data = reader.read_raw(data.to_vec()).ok()?;

    for field in exif_data.fields() {
        if field.tag == exif::Tag::JPEGInterchangeFormat {
            if let exif::Value::Long(ref offsets) = field.value {
                if let Some(&offset) = offsets.first() {
                    for len_field in exif_data.fields() {
                        if len_field.tag == exif::Tag::JPEGInterchangeFormatLength {
                            if let exif::Value::Long(ref lengths) = len_field.value {
                                if let Some(&length) = lengths.first() {
                                    let start = offset as usize;
                                    let end = start + length as usize;
                                    if end <= data.len() {
                                        let thumb_data = &data[start..end];
                                        if thumb_data.starts_with(&[0xFF, 0xD8]) {
                                            if let Some((w, h)) = jpeg_dimensions(thumb_data) {
                                                return fit_existing_or_resize_jpeg(
                                                    thumb_data.to_vec(),
                                                    w,
                                                    h,
                                                    max_dimension,
                                                    ThumbnailSource::EmbeddedPreview,
                                                    ThumbnailSource::EmbeddedPreview,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

fn existing_jpeg_thumbnail(
    data: Vec<u8>,
    width: u32,
    height: u32,
    source: ThumbnailSource,
) -> ThumbnailResult {
    ThumbnailResult {
        data,
        format: ImageFormat::Jpeg,
        width,
        height,
        source,
    }
}

pub(crate) fn fit_existing_or_resize_jpeg(
    data: Vec<u8>,
    width: u32,
    height: u32,
    max_dimension: u32,
    passthrough_source: ThumbnailSource,
    resize_source: ThumbnailSource,
) -> Option<ThumbnailResult> {
    if width <= max_dimension && height <= max_dimension {
        return Some(existing_jpeg_thumbnail(
            data,
            width,
            height,
            passthrough_source,
        ));
    }

    let mut resized = decode_and_resize_fast_jpeg(&data, max_dimension)?;
    resized.source = resize_source;
    Some(resized)
}

fn is_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF
    )
}

fn decode_and_resize_fast_jpeg(data: &[u8], max_dimension: u32) -> Option<ThumbnailResult> {
    decode_and_resize_fast(Cursor::new(data), max_dimension)
}

fn decode_and_resize_from_path_fast_jpeg(
    file_path: &Path,
    max_dimension: u32,
) -> Option<ThumbnailResult> {
    decode_and_resize_fast(BufReader::new(File::open(file_path).ok()?), max_dimension)
}

/// Decodes at the smallest DCT scale that still covers `max_dimension`, then resizes.
fn decode_and_resize_fast(
    reader: impl std::io::Read,
    max_dimension: u32,
) -> Option<ThumbnailResult> {
    let mut decoder = JpegFastDecoder::new(reader);
    let req = max_dimension.min(u16::MAX as u32) as u16;
    let _ = decoder.scale(req, req).ok()?;
    let pixels = decoder.decode().ok()?;
    let info = decoder.info()?;
    let decoded = dynamic_from_fast_pixels(
        info.width as u32,
        info.height as u32,
        info.pixel_format,
        pixels,
    )?;
    finalize_fast_decoded_image(decoded, max_dimension)
}

fn finalize_fast_decoded_image(
    decoded: image::DynamicImage,
    max_dimension: u32,
) -> Option<ThumbnailResult> {
    let resized = if decoded.width() > max_dimension || decoded.height() > max_dimension {
        decoded.thumbnail(max_dimension, max_dimension)
    } else {
        decoded
    };

    let (w, h) = (resized.width(), resized.height());
    let encoded = encode_thumbnail_jpeg_fast(&resized)?;

    Some(ThumbnailResult {
        data: encoded,
        format: ImageFormat::Jpeg,
        width: w,
        height: h,
        source: ThumbnailSource::PartialDecode,
    })
}

fn dynamic_from_fast_pixels(
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    pixels: Vec<u8>,
) -> Option<image::DynamicImage> {
    match pixel_format {
        PixelFormat::L8 => {
            let img = image::GrayImage::from_raw(width, height, pixels)?;
            Some(image::DynamicImage::ImageLuma8(img))
        }
        PixelFormat::RGB24 => {
            let img = image::RgbImage::from_raw(width, height, pixels)?;
            Some(image::DynamicImage::ImageRgb8(img))
        }
        PixelFormat::CMYK32 => {
            let rgb = cmyk_to_rgb(&pixels);
            let img = image::RgbImage::from_raw(width, height, rgb)?;
            Some(image::DynamicImage::ImageRgb8(img))
        }
        PixelFormat::L16 => {
            if !pixels.len().is_multiple_of(2) {
                return None;
            }
            let gray: Vec<u8> = pixels.chunks_exact(2).map(|chunk| chunk[0]).collect();
            let img = image::GrayImage::from_raw(width, height, gray)?;
            Some(image::DynamicImage::ImageLuma8(img))
        }
    }
}

fn cmyk_to_rgb(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len().saturating_sub(input.len() / 4));

    for pixel in input.chunks_exact(4) {
        let c = pixel[0] as f32 / 255.0;
        let m = pixel[1] as f32 / 255.0;
        let y = pixel[2] as f32 / 255.0;
        let k = pixel[3] as f32 / 255.0;

        let c = c * (1.0 - k) + k;
        let m = m * (1.0 - k) + k;
        let y = y * (1.0 - k) + k;

        output.push(((1.0 - c) * 255.0) as u8);
        output.push(((1.0 - m) * 255.0) as u8);
        output.push(((1.0 - y) * 255.0) as u8);
    }

    output
}

fn encode_thumbnail_jpeg_fast(img: &image::DynamicImage) -> Option<Vec<u8>> {
    // Thumbnails favor speed; preview quality is handled by the preview pipeline.
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 60);
    encoder.encode_image(img).ok()?;
    Some(out)
}

fn read_head(path: &Path, max_len: usize) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; max_len];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}
