//! Decoders for formats which are not handled by `image`'s normal reader.
//!
//! Keep the dispatch signature-based so remote objects work even when their
//! name or Content-Type is missing or incorrect.

use image::DynamicImage;
use std::collections::HashMap;
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
    // JPEG XR uses a little-endian TIFF-like header. The bundled decoder only
    // accepts `II`; a big-endian `MM` signature is not a supported JPEG XR file
    // and must not be handed to the native decoder.
    if data.starts_with(&[b'I', b'I', 0xbc, 0x01]) {
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
    if !j2k_tlm_sot_consistent(data) {
        return None;
    }
    let decoded = jpeg2k::Image::from_bytes(data).ok()?;
    DynamicImage::try_from(&decoded).ok()
}

/// OSV-2025-219: unpatched OpenJPEG sizes a tile's tile-part index from the
/// number of TLM entries for that tile, then writes through the SOT's TPsot
/// without a bounds check. Reject that inconsistent pair in Rust before the
/// decoder is called. Only the main and tile-part headers are inspected;
/// Psot skips tile data, where marker-like bytes must not be interpreted.
fn j2k_tlm_sot_consistent(data: &[u8]) -> bool {
    let Some(codestream) = jpeg2000_codestream(data) else {
        return false;
    };
    if !codestream.starts_with(&[0xff, 0x4f]) {
        return false;
    }

    let mut pos = 2usize; // after SOC
    let mut tile_end = 0usize;
    let mut in_tile_part = false;
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut entries_seen = 0u32;
    let mut saw_tlm = false;
    let mut tlm_invalid = false;

    while pos < codestream.len() {
        if codestream[pos] != 0xff || pos + 2 > codestream.len() {
            return false;
        }
        let marker = u16::from_be_bytes([codestream[pos], codestream[pos + 1]]);
        match marker {
            0xffd9 => return true, // EOC
            0xff93 => {
                // SOD: skip the tile-part body declared by the preceding SOT.
                if !in_tile_part || pos + 2 > tile_end {
                    return false;
                }
                pos = tile_end;
                in_tile_part = false;
                continue;
            }
            0xff4f => return false,                 // duplicate SOC
            0xff90 if in_tile_part => return false, // SOT before SOD
            _ => {}
        }

        if pos + 4 > codestream.len() {
            return false;
        }
        let length = usize::from(u16::from_be_bytes([
            codestream[pos + 2],
            codestream[pos + 3],
        ]));
        if length < 2 {
            return false;
        }
        let Some(end) = pos
            .checked_add(2)
            .and_then(|value| value.checked_add(length))
        else {
            return false;
        };
        let limit = if in_tile_part {
            tile_end
        } else {
            codestream.len()
        };
        if end > limit {
            return false;
        }

        match marker {
            0xff55 => {
                // TLM: OpenJPEG ignores all TLM data after an invalid segment.
                if !tlm_invalid {
                    saw_tlm = true;
                    if !read_tlm(&codestream[pos + 4..end], &mut counts, &mut entries_seen) {
                        tlm_invalid = true;
                        counts.clear();
                    }
                }
            }
            0xff90 => {
                // SOT: Lsot is fixed at 10, so the marker segment is 12 bytes.
                if length != 10 {
                    return false;
                }
                let tile = u32::from(u16::from_be_bytes([
                    codestream[pos + 4],
                    codestream[pos + 5],
                ]));
                let psot = u32::from_be_bytes([
                    codestream[pos + 6],
                    codestream[pos + 7],
                    codestream[pos + 8],
                    codestream[pos + 9],
                ]);
                let tpsot = u32::from(codestream[pos + 10]);
                if saw_tlm && !tlm_invalid && tpsot >= counts.get(&tile).copied().unwrap_or(0) {
                    return false;
                }
                if psot == 0 {
                    // A zero Psot declares the last tile-part; its data extends
                    // to EOC, so there are no further headers to compare.
                    return true;
                }
                let Some(declared_end) = (pos as u64)
                    .checked_add(u64::from(psot))
                    .and_then(|value| usize::try_from(value).ok())
                else {
                    return false;
                };
                if declared_end < end + 2 || declared_end > codestream.len() {
                    return false;
                }
                tile_end = declared_end;
                in_tile_part = true;
            }
            _ => {}
        }
        pos = end;
    }

    // A SOT whose SOD is missing has no bounded tile-part body to skip.
    !in_tile_part
}

/// Counts TLM tile-part entries like unpatched OpenJPEG's `opj_j2k_read_tlm`:
/// ST selects the tile-index width, SP selects the Ptlm width, and ST == 0
/// uses a running entry counter as the tile index. False invalidates TLM.
fn read_tlm(payload: &[u8], counts: &mut HashMap<u32, u32>, entries_seen: &mut u32) -> bool {
    if payload.len() < 2 {
        return false;
    }
    let stlm = payload[1];
    let st = (stlm >> 4) & 3;
    if st == 3 {
        return false;
    }
    let sp = (stlm >> 6) & 1;
    let entry_size = (usize::from(sp) + 1) * 2 + usize::from(st);
    let entries = &payload[2..];
    if entries.is_empty() || !entries.len().is_multiple_of(entry_size) {
        return false;
    }
    for entry in entries.chunks_exact(entry_size) {
        let tile = if st == 0 {
            *entries_seen
        } else {
            let mut tile = 0u32;
            for byte in &entry[..usize::from(st)] {
                tile = (tile << 8) | u32::from(*byte);
            }
            tile
        };
        *counts.entry(tile).or_insert(0) += 1;
        *entries_seen = entries_seen.saturating_add(1);
    }
    true
}

/// Returns the raw J2K codestream, or the `jp2c` box payload of a JP2 file.
fn jpeg2000_codestream(data: &[u8]) -> Option<&[u8]> {
    if data.starts_with(&[0xff, 0x4f]) {
        return Some(data);
    }
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?);
        let kind = &data[pos + 4..pos + 8];
        let (header, end) = match size {
            0 => (8usize, data.len()),
            1 => {
                if pos + 16 > data.len() {
                    return None;
                }
                let large = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().ok()?);
                (16usize, usize::try_from(large).ok()?.checked_add(pos + 16)?)
            }
            size => {
                if size < 8 {
                    return None;
                }
                (8usize, (size as usize).checked_add(pos)?)
            }
        };
        if end > data.len() || end < pos + header {
            return None;
        }
        if kind == b"jp2c" {
            return Some(&data[pos + header..end]);
        }
        if size == 0 {
            break;
        }
        pos = end;
    }
    None
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

    let plane = Plane {
        data: &pixels,
        width,
        height,
        stride,
    };
    let mut rgba = match format {
        PixelFormat::PixelFormatBlackWhite => unpack_monochrome(&pixels, width, height, stride),
        PixelFormat::PixelFormat16bppRGB555 => map_pixels(plane, 2, |px| Some(rgb16(px, false))),
        PixelFormat::PixelFormat16bppRGB565 => map_pixels(plane, 2, |px| Some(rgb16(px, true))),
        PixelFormat::PixelFormat32bppRGB101010 => map_pixels(plane, 4, rgb101010),
        _ if info.bit_depth() == BitDepthBits::Eight => {
            unpack_channels(plane, &info, 1, |s| s[0], |s| s[0])
        }
        _ if info.bit_depth() == BitDepthBits::Sixteen => {
            let to_u8 =
                |s: &[u8]| ((u32::from(u16::from_le_bytes([s[0], s[1]])) + 128) / 257) as u8;
            unpack_channels(plane, &info, 2, to_u8, to_u8)
        }
        _ if info.bit_depth() == BitDepthBits::ThirtyTwoF
            && matches!(info.color_format(), ColorFormat::RGB | ColorFormat::YOnly) =>
        {
            let value = |s: &[u8]| f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
            // scRGB is linear and may exceed 1.0. Reinhard keeps HDR highlights visible.
            let tone_map = |s: &[u8]| {
                let linear = value(s).max(0.0);
                ((linear / (1.0 + linear)).powf(1.0 / 2.2) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            let alpha = |s: &[u8]| (value(s).clamp(0.0, 1.0) * 255.0) as u8;
            unpack_channels(plane, &info, 4, tone_map, alpha)
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

/// Decoded pixel rows as copied out of the decoder.
#[derive(Clone, Copy)]
struct Plane<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
    stride: usize,
}

/// Converts every `bytes_per_pixel`-wide pixel of each row to RGBA.
fn map_pixels(
    Plane {
        data,
        width,
        height,
        stride,
    }: Plane<'_>,
    bytes_per_pixel: usize,
    mut convert: impl FnMut(&[u8]) -> Option<[u8; 4]>,
) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?,
    );
    for row in data.chunks_exact(stride).take(height as usize) {
        for px in row.chunks_exact(bytes_per_pixel).take(width as usize) {
            out.extend_from_slice(&convert(px)?);
        }
    }
    Some(out)
}

fn rgb16(px: &[u8], rgb565: bool) -> [u8; 4] {
    let value = u16::from_le_bytes([px[0], px[1]]);
    let (r5, g, b5, gbits) = if rgb565 {
        ((value >> 11) & 31, (value >> 5) & 63, value & 31, 63)
    } else {
        ((value >> 10) & 31, (value >> 5) & 31, value & 31, 31)
    };
    [
        ((r5 * 255 + 15) / 31) as u8,
        ((g * 255 + gbits / 2) / gbits) as u8,
        ((b5 * 255 + 15) / 31) as u8,
        255,
    ]
}

fn rgb101010(px: &[u8]) -> Option<[u8; 4]> {
    let value = u32::from_le_bytes(px.try_into().ok()?);
    let scale = |component: u32| ((component * 255 + 511) / 1023) as u8;
    Some([
        scale((value >> 20) & 0x3ff),
        scale((value >> 10) & 0x3ff),
        scale(value & 0x3ff),
        255,
    ])
}

/// Gray, RGB(A) or CMYK(A) pixels whose channels are `unit` bytes wide; `color` and `alpha`
/// convert one channel to 8 bits.
fn unpack_channels(
    plane: Plane<'_>,
    info: &jpegxr::PixelInfo,
    unit: usize,
    color: impl Fn(&[u8]) -> u8,
    alpha: impl Fn(&[u8]) -> u8,
) -> Option<Vec<u8>> {
    use jpegxr::ColorFormat;
    let channels = info.channels();
    if channels == 0 || channels > 5 {
        return None;
    }
    let bytes_per_pixel = info.bits_per_pixel().checked_add(7)? / 8;
    map_pixels(plane, bytes_per_pixel, |px| {
        let channel = |index: usize| px.get(index * unit..(index + 1) * unit);
        let c = |index: usize| channel(index).map(&color);
        Some(match info.color_format() {
            ColorFormat::YOnly => {
                let y = c(0)?;
                [y, y, y, 255]
            }
            ColorFormat::RGB if channels >= 3 => {
                let (r, b) = if info.bgr() { (2, 0) } else { (0, 2) };
                let a = if info.has_alpha() {
                    channel(channels - 1).map(&alpha)?
                } else {
                    255
                };
                [c(r)?, c(1)?, c(b)?, a]
            }
            ColorFormat::CMYK if channels >= 4 => {
                let k = 255 - u16::from(c(3)?);
                let convert =
                    |component: u8| (((255 - u16::from(component)) * k + 127) / 255) as u8;
                let a = if info.has_alpha() && channels >= 5 {
                    channel(4).map(&alpha)?
                } else {
                    255
                };
                [convert(c(0)?), convert(c(1)?), convert(c(2)?), a]
            }
            _ => return None,
        })
    })
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

    /// Adds one TLM entry for tile 0 to the fixture's main header and sets
    /// the SOT's TPsot. Ptlm is the fixture's Psot (15 bytes).
    fn fixture_with_tlm(tpsot: u8) -> Vec<u8> {
        let jp2 = include_bytes!("../tests/fixtures/gray-1x1.jp2");
        let j2k = &jp2[85..]; // Payload of the fixture's jp2c box.
        let sot = j2k
            .windows(2)
            .position(|marker| marker == [0xff, 0x90])
            .unwrap_or(0);
        assert!(sot > 0, "fixture has no SOT marker");
        let ptlm = u16::from_be_bytes([j2k[sot + 8], j2k[sot + 9]]);

        let mut codestream = Vec::with_capacity(j2k.len() + 9);
        codestream.extend_from_slice(&j2k[..sot]);
        // ST=1 (one-byte tile index), SP=0 (two-byte Ptlm), one entry.
        codestream.extend_from_slice(&[0xff, 0x55, 0x00, 0x07, 0x00, 0x10, 0x00]);
        codestream.extend_from_slice(&ptlm.to_be_bytes());
        codestream.extend_from_slice(&j2k[sot..]);
        // Nine bytes of TLM were inserted before SOT; TPsot is 10 bytes
        // after the SOT marker.
        codestream[sot + 9 + 10] = tpsot;

        let mut wrapped = Vec::with_capacity(jp2.len() + 9);
        wrapped.extend_from_slice(&jp2[..77]);
        wrapped.extend_from_slice(&((8 + codestream.len()) as u32).to_be_bytes());
        wrapped.extend_from_slice(b"jp2c");
        wrapped.extend_from_slice(&codestream);
        wrapped
    }

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
        // Big-endian JXR signature is unsupported; routing it to the native
        // decoder caused an ASan stack-buffer-overflow in image_sniff.
        assert_eq!(detect(&[b'M', b'M', 0x01, 0xbc]), None);

        let mut heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec();
        heic.resize(32, 0);
        assert_eq!(detect(&heic), Some(ModernFormat::Heic));
    }

    #[test]
    fn decodes_jpeg2000_preview() {
        // Generated with ImageMagick: -size 1x1 xc:'#808080' -colorspace Gray -depth 8 -strip.
        let jp2 = include_bytes!("../tests/fixtures/gray-1x1.jp2");
        let j2k = &jp2[85..]; // Payload of the fixture's jp2c box.
        for bytes in [jp2.as_slice(), j2k] {
            let decoded = decode_bytes(bytes).expect("JPEG 2000 preview").to_luma8();
            assert_eq!(decoded.dimensions(), (1, 1));
            assert_eq!(decoded.as_raw(), &[128]);
        }
    }

    #[test]
    fn rejects_truncated_jpeg2000_codestream() {
        // Recognizable SOC and SIZ markers, but the SIZ segment is missing.
        assert!(decode_bytes(&[0xff, 0x4f, 0xff, 0x51]).is_none());
    }

    #[test]
    fn tlm_sot_inconsistency_is_rejected_before_decode() {
        // TLM lists one tile-part for tile 0, but SOT claims TPsot=1.
        let jp2 = fixture_with_tlm(1);
        assert!(!j2k_tlm_sot_consistent(&jp2));
        assert!(decode_bytes(&jp2).is_none());
        // The same fixture with TPsot=0 differs only in that byte.
        assert!(j2k_tlm_sot_consistent(&fixture_with_tlm(0)));
    }

    #[test]
    fn tlm_sot_consistent_pair_is_accepted() {
        let jp2 = fixture_with_tlm(0);
        assert!(j2k_tlm_sot_consistent(&jp2));
        let decoded = decode_bytes(&jp2).expect("TLM fixture decodes").to_luma8();
        assert_eq!(decoded.dimensions(), (1, 1));
        assert_eq!(decoded.as_raw(), &[128]);
    }

    #[test]
    fn tlm_absent_codestream_is_preserved() {
        let jp2 = include_bytes!("../tests/fixtures/gray-1x1.jp2");
        assert!(j2k_tlm_sot_consistent(jp2));
        assert!(j2k_tlm_sot_consistent(&jp2[85..]));
    }

    #[test]
    fn tlm_marker_length_overrun_is_rejected() {
        let jp2 = include_bytes!("../tests/fixtures/gray-1x1.jp2");
        let mut j2k = jp2[85..].to_vec();
        // TLM at offset 45 with LTLM claiming more bytes than remain before SOT.
        j2k.splice(45..45, [0xff, 0x55, 0x7f, 0xff]);
        assert!(!j2k_tlm_sot_consistent(&j2k));
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
    fn jpeg_xr_decoder_releases_after_failed_initialization() {
        // Workspace-crate guard for the vendored decoder-ownership fix. The
        // counters observe C allocation and Release calls, so reverting the
        // error-path ownership cannot silently pass this test.
        let (created_before, released_before) = jpegxr::decoder_counts();
        let result = jpegxr::ImageDecode::with_reader(std::io::Cursor::new([0u8; 2]));
        assert!(matches!(result, Err(jpegxr::JXRError::UnsupportedFormat)));
        let (created_after, released_after) = jpegxr::decoder_counts();
        assert!(
            created_after > created_before,
            "invalid input did not allocate a C decoder"
        );
        assert_eq!(
            created_after - created_before,
            released_after - released_before,
            "decoder allocated during failed initialization was not released"
        );
    }

    #[test]
    fn unpremultiplies_jpeg_xr_alpha() {
        let mut pixels = [64, 32, 16, 128, 20, 30, 40, 255];
        unpremultiply_rgba(&mut pixels);
        assert_eq!(pixels, [128, 64, 32, 128, 20, 30, 40, 255]);
    }
}
