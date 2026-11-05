//! Decoders for formats which are not handled by `image`'s normal reader.
//!
//! Keep the dispatch signature-based so remote objects work even when their
//! name or Content-Type is missing or incorrect.

use image::DynamicImage;
use std::io::Cursor;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModernFormat {
    Avif,
    Jpeg2000,
    JpegXr,
    JpegXl,
    Heic,
}

pub(crate) fn decode_path(path: &Path) -> Option<DynamicImage> {
    let data = std::fs::read(path).ok()?;
    decode_bytes(&data)
}

pub(crate) fn decode_bytes(data: &[u8]) -> Option<DynamicImage> {
    match detect(data)? {
        ModernFormat::Avif => decode_avif(data),
        ModernFormat::Jpeg2000 => decode_jpeg2000(data),
        ModernFormat::JpegXr => decode_jpeg_xr(data),
        ModernFormat::JpegXl => decode_jpeg_xl(data),
        ModernFormat::Heic => decode_heic(data),
    }
}

pub(crate) fn ffmpeg_extension(data: &[u8]) -> Option<&'static str> {
    match detect(data) {
        Some(ModernFormat::Avif) => Some("avif"),
        Some(ModernFormat::Jpeg2000) => Some("jp2"),
        Some(ModernFormat::JpegXr) => Some("jxr"),
        Some(ModernFormat::JpegXl) => Some("jxl"),
        Some(ModernFormat::Heic) => Some("heic"),
        None => None,
    }
}

fn detect(data: &[u8]) -> Option<ModernFormat> {
    if is_avif_container(data) {
        return Some(ModernFormat::Avif);
    }
    if data.starts_with(&[0xff, 0x4f, 0xff, 0x51])
        || data.starts_with(&[0, 0, 0, 12, b'j', b'P', b' ', b' ', 13, 10, 0x87, 10])
    {
        return Some(ModernFormat::Jpeg2000);
    }
    if data.starts_with(&[0xff, 0x0a])
        || data.starts_with(&[0, 0, 0, 12, b'J', b'X', b'L', b' ', 13, 10, 0x87, 10])
    {
        return Some(ModernFormat::JpegXl);
    }
    // JPEG XR uses a TIFF-like header in either byte order.
    if data.starts_with(&[b'I', b'I', 0xbc, 0x01]) || data.starts_with(&[b'M', b'M', 0x01, 0xbc]) {
        return Some(ModernFormat::JpegXr);
    }
    if is_heic_container(data) {
        return Some(ModernFormat::Heic);
    }
    None
}

fn is_heic_container(data: &[u8]) -> bool {
    // Major and compatible brands used by HEVC-based HEIF still images.
    // `mif1`/`msf1` alone are generic, so require an HEVC-specific brand.
    has_ftyp_brand(
        data,
        &[
            *b"heic", *b"heix", *b"hevc", *b"hevx", *b"heim", *b"heis", *b"hevm", *b"hevs",
        ],
    )
}

fn is_avif_container(data: &[u8]) -> bool {
    has_ftyp_brand(data, &[*b"avif", *b"avis"])
}

fn has_ftyp_brand(data: &[u8], expected: &[[u8; 4]]) -> bool {
    if data.len() < 16 || &data[4..8] != b"ftyp" {
        return false;
    }

    let (box_size, brand_offset) = match u32::from_be_bytes([data[0], data[1], data[2], data[3]]) {
        0 => (data.len(), 8),
        1 if data.len() >= 24 => {
            let large_size = u64::from_be_bytes([
                data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
            ]);
            let Ok(box_size) = usize::try_from(large_size) else {
                return false;
            };
            (box_size, 16)
        }
        1 => return false,
        size => (size as usize, 8),
    };
    let box_end = box_size.min(data.len());
    if box_end < brand_offset + 8 {
        return false;
    }

    let major = &data[brand_offset..brand_offset + 4];
    if expected.iter().any(|brand| major == brand) {
        return true;
    }

    // Skip the 4-byte minor version between the major and compatible brands.
    data[brand_offset + 8..box_end]
        .chunks_exact(4)
        .any(|brand| expected.iter().any(|expected| brand == expected))
}

fn decode_jpeg2000(data: &[u8]) -> Option<DynamicImage> {
    let decoded = jpeg2k::Image::from_bytes(data).ok()?;
    DynamicImage::try_from(&decoded).ok()
}

fn decode_avif(data: &[u8]) -> Option<DynamicImage> {
    let decoded = avif_rust::image_from_bytes(data).ok()?;
    let width = u32::try_from(decoded.width).ok()?;
    let height = u32::try_from(decoded.height).ok()?;
    image::RgbaImage::from_raw(width, height, decoded.rgba).map(DynamicImage::ImageRgba8)
}

fn decode_jpeg_xl(data: &[u8]) -> Option<DynamicImage> {
    let decoder = jxl_oxide::integration::JxlDecoder::new(Cursor::new(data)).ok()?;
    DynamicImage::from_decoder(decoder).ok()
}

fn decode_heic(data: &[u8]) -> Option<DynamicImage> {
    let decoded = heif_oxide::decode_bytes(data).ok()?;
    let rgba = image::RgbaImage::from_raw(decoded.width, decoded.height, decoded.to_rgba8())?;
    Some(DynamicImage::ImageRgba8(rgba))
}

fn decode_jpeg_xr(data: &[u8]) -> Option<DynamicImage> {
    use jpegxr::{BitDepthBits, ColorFormat, ImageDecode, PixelFormat, PixelInfo};

    let mut decoder = ImageDecode::with_reader(Cursor::new(data)).ok()?;
    let (width, height) = decoder.get_size().ok()?;
    let (width, height) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    if width == 0 || height == 0 {
        return None;
    }

    let format = decoder.get_pixel_format().ok()?;
    let info = PixelInfo::from_format(format);
    let row_bits = (width as usize).checked_mul(info.bits_per_pixel())?;
    let stride = row_bits.checked_add(7)? / 8;
    let mut pixels = vec![0u8; stride.checked_mul(height as usize)?];
    decoder.copy_all(&mut pixels, stride).ok()?;

    let mut rgba = match format {
        PixelFormat::PixelFormatBlackWhite => unpack_monochrome(&pixels, width, height, stride),
        PixelFormat::PixelFormat16bppRGB555 => unpack_rgb16(&pixels, width, height, stride, false),
        PixelFormat::PixelFormat16bppRGB565 => unpack_rgb16(&pixels, width, height, stride, true),
        PixelFormat::PixelFormat32bppRGB101010 => unpack_rgb101010(&pixels, width, height, stride),
        _ if info.bit_depth() == BitDepthBits::Eight => {
            unpack_u8(&pixels, width, height, stride, &info)
        }
        _ if info.bit_depth() == BitDepthBits::Sixteen => {
            unpack_u16(&pixels, width, height, stride, &info)
        }
        _ if info.bit_depth() == BitDepthBits::ThirtyTwoF
            && matches!(info.color_format(), ColorFormat::RGB | ColorFormat::YOnly) =>
        {
            unpack_f32(&pixels, width, height, stride, &info)
        }
        _ => None,
    }?;

    if info.premultiplied_alpha() {
        unpremultiply_rgba(&mut rgba);
    }

    image::RgbaImage::from_raw(width, height, rgba).map(DynamicImage::ImageRgba8)
}

fn unpack_monochrome(data: &[u8], width: u32, height: u32, stride: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?,
    );
    for row in data.chunks_exact(stride).take(height as usize) {
        for x in 0..width as usize {
            let value = if row[x / 8] & (0x80 >> (x % 8)) == 0 {
                0
            } else {
                255
            };
            out.extend_from_slice(&[value, value, value, 255]);
        }
    }
    Some(out)
}

fn unpack_rgb16(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    rgb565: bool,
) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?,
    );
    for row in data.chunks_exact(stride).take(height as usize) {
        for px in row.chunks_exact(2).take(width as usize) {
            let value = u16::from_le_bytes([px[0], px[1]]);
            let (r5, g, b5, gbits) = if rgb565 {
                ((value >> 11) & 31, (value >> 5) & 63, value & 31, 63)
            } else {
                ((value >> 10) & 31, (value >> 5) & 31, value & 31, 31)
            };
            out.extend_from_slice(&[
                ((r5 * 255 + 15) / 31) as u8,
                ((g * 255 + gbits / 2) / gbits) as u8,
                ((b5 * 255 + 15) / 31) as u8,
                255,
            ]);
        }
    }
    Some(out)
}

fn unpack_rgb101010(data: &[u8], width: u32, height: u32, stride: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?,
    );
    for row in data.chunks_exact(stride).take(height as usize) {
        for px in row.chunks_exact(4).take(width as usize) {
            let value = u32::from_le_bytes(px.try_into().ok()?);
            let b = value & 0x3ff;
            let g = (value >> 10) & 0x3ff;
            let r = (value >> 20) & 0x3ff;
            out.extend_from_slice(&[
                ((r * 255 + 511) / 1023) as u8,
                ((g * 255 + 511) / 1023) as u8,
                ((b * 255 + 511) / 1023) as u8,
                255,
            ]);
        }
    }
    Some(out)
}

fn unpack_u8(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    info: &jpegxr::PixelInfo,
) -> Option<Vec<u8>> {
    use jpegxr::ColorFormat;
    let channels = info.channels();
    let bytes_per_pixel = info.bits_per_pixel().checked_add(7)? / 8;
    if channels == 0 || channels > 5 {
        return None;
    }
    let mut out = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?,
    );
    for row in data.chunks_exact(stride).take(height as usize) {
        for px in row.chunks_exact(bytes_per_pixel).take(width as usize) {
            let rgba = match info.color_format() {
                ColorFormat::YOnly => [px[0], px[0], px[0], 255],
                ColorFormat::RGB if channels >= 3 => {
                    let (r, b) = if info.bgr() {
                        (px[2], px[0])
                    } else {
                        (px[0], px[2])
                    };
                    [
                        r,
                        px[1],
                        b,
                        if info.has_alpha() {
                            px[channels - 1]
                        } else {
                            255
                        },
                    ]
                }
                ColorFormat::CMYK if channels >= 4 => {
                    let k = 255u16 - px[3] as u16;
                    let convert =
                        |component: u8| (((255u16 - component as u16) * k + 127) / 255) as u8;
                    [
                        convert(px[0]),
                        convert(px[1]),
                        convert(px[2]),
                        if info.has_alpha() && channels >= 5 {
                            px[4]
                        } else {
                            255
                        },
                    ]
                }
                _ => return None,
            };
            out.extend_from_slice(&rgba);
        }
    }
    Some(out)
}

fn unpack_u16(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    info: &jpegxr::PixelInfo,
) -> Option<Vec<u8>> {
    use jpegxr::ColorFormat;
    let channels = info.channels();
    let bytes_per_pixel = info.bits_per_pixel().checked_add(7)? / 8;
    if channels == 0 || channels > 5 {
        return None;
    }
    let mut out = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?,
    );
    for row in data.chunks_exact(stride).take(height as usize) {
        for px in row.chunks_exact(bytes_per_pixel).take(width as usize) {
            let sample = |idx: usize| u16::from_le_bytes([px[idx * 2], px[idx * 2 + 1]]);
            let to_u8 = |value: u16| ((value as u32 + 128) / 257) as u8;
            let rgba = match info.color_format() {
                ColorFormat::YOnly => {
                    let y = to_u8(sample(0));
                    [y, y, y, 255]
                }
                ColorFormat::RGB if channels >= 3 => {
                    let (r, b) = if info.bgr() {
                        (sample(2), sample(0))
                    } else {
                        (sample(0), sample(2))
                    };
                    [
                        r,
                        sample(1),
                        b,
                        if info.has_alpha() {
                            sample(channels - 1)
                        } else {
                            u16::MAX
                        },
                    ]
                    .map(to_u8)
                }
                _ => return None,
            };
            out.extend_from_slice(&rgba);
        }
    }
    Some(out)
}

fn unpack_f32(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    info: &jpegxr::PixelInfo,
) -> Option<Vec<u8>> {
    use jpegxr::ColorFormat;
    let channels = info.channels();
    let bytes_per_pixel = info.bits_per_pixel().checked_add(7)? / 8;
    if channels == 0 || channels > 4 {
        return None;
    }
    let mut out = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?,
    );
    let map = |value: f32| {
        // scRGB is linear and may exceed 1.0. Reinhard keeps HDR highlights visible.
        let linear = value.max(0.0);
        let mapped = linear / (1.0 + linear);
        (mapped.powf(1.0 / 2.2) * 255.0).round().clamp(0.0, 255.0) as u8
    };
    for row in data.chunks_exact(stride).take(height as usize) {
        for px in row.chunks_exact(bytes_per_pixel).take(width as usize) {
            let sample = |idx: usize| {
                let offset = idx * 4;
                f32::from_le_bytes([px[offset], px[offset + 1], px[offset + 2], px[offset + 3]])
            };
            let rgba = match info.color_format() {
                ColorFormat::YOnly => {
                    let y = map(sample(0));
                    [y, y, y, 255]
                }
                ColorFormat::RGB if channels >= 3 => {
                    let (r, b) = if info.bgr() {
                        (sample(2), sample(0))
                    } else {
                        (sample(0), sample(2))
                    };
                    [
                        map(r),
                        map(sample(1)),
                        map(b),
                        if info.has_alpha() {
                            (sample(channels - 1).clamp(0.0, 1.0) * 255.0) as u8
                        } else {
                            255
                        },
                    ]
                }
                _ => return None,
            };
            out.extend_from_slice(&rgba);
        }
    }
    Some(out)
}

fn unpremultiply_rgba(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_all_container_signatures() {
        assert_eq!(
            detect(&[0xff, 0x4f, 0xff, 0x51]),
            Some(ModernFormat::Jpeg2000)
        );
        assert_eq!(detect(&[0xff, 0x0a]), Some(ModernFormat::JpegXl));
        assert_eq!(
            detect(&[b'I', b'I', 0xbc, 0x01]),
            Some(ModernFormat::JpegXr)
        );

        let mut heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec();
        heic.resize(32, 0);
        assert_eq!(detect(&heic), Some(ModernFormat::Heic));
    }

    #[test]
    fn does_not_confuse_avif_with_heic() {
        let avif = b"\0\0\0\x18ftypavif\0\0\0\0mif1avif";
        assert_eq!(detect(avif), Some(ModernFormat::Avif));
    }

    #[test]
    fn detects_compatible_brand_in_extended_ftyp_box() {
        let avif = b"\0\0\0\x01ftyp\0\0\0\0\0\0\0\x20mif1\0\0\0\0avifmif1";
        assert_eq!(detect(avif), Some(ModernFormat::Avif));
    }

    #[test]
    fn unpremultiplies_jpeg_xr_alpha() {
        let mut pixels = [64, 32, 16, 128, 20, 30, 40, 255];
        unpremultiply_rgba(&mut pixels);
        assert_eq!(pixels, [128, 64, 32, 128, 20, 30, 40, 255]);
    }
}
