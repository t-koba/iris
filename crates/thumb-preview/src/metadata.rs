use crate::io::binary_reader::{join_to_backend_error, RemoteRangeReader, RemoteWindowSource};
use crate::raw::{extract_raw_metadata, is_known_raw_extension, supports_raw_extension};
use chrono::NaiveDateTime;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::metadata::MediaMetadata;
use iris_core_domain::types::object::ObjectRef;
use std::collections::HashSet;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;

const X3F_SIGNATURE: &[u8; 4] = b"FOVb";
const X3F_FILE_VERSION_OFFSET: u64 = 4;
const X3F_IMAGE_WIDTH_OFFSET: u64 = 0x28;
const X3F_IMAGE_HEIGHT_OFFSET: u64 = 0x2C;
const X3F_ROTATION_OFFSET: u64 = 0x30;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub media: MediaMetadata,
    pub exif_entries: Vec<(String, String)>,
}

#[derive(Default)]
struct ExtractedExifMetadata {
    width: Option<u32>,
    height: Option<u32>,
    media: MediaMetadata,
    entries: Vec<(String, String)>,
}

pub fn extract_image_metadata_from_path(file_path: &Path) -> ImageMetadata {
    if is_raw_path(file_path) {
        return extract_raw_metadata(file_path)
            .map(|metadata| ImageMetadata {
                width: metadata.width,
                height: metadata.height,
                media: metadata.media,
                exif_entries: metadata.exif_entries,
            })
            .unwrap_or_default();
    }

    let mut metadata = ImageMetadata::default();
    if let Ok(file) = std::fs::File::open(file_path) {
        if let Some((width, height)) = probe_image_dimensions(file) {
            metadata.width = Some(width);
            metadata.height = Some(height);
        }
    }

    if let Ok(file) = std::fs::File::open(file_path) {
        let extracted = extract_filtered_exif_metadata(file);
        if metadata.width.is_none() {
            metadata.width = extracted.width;
        }
        if metadata.height.is_none() {
            metadata.height = extracted.height;
        }
        metadata.media = extracted.media;
        if metadata.media.width.is_none() {
            metadata.media.width = metadata.width;
        }
        if metadata.media.height.is_none() {
            metadata.media.height = metadata.height;
        }
        metadata.exif_entries = extracted.entries;
    }

    metadata
}

pub async fn extract_image_metadata_from_backend(
    backend: Arc<dyn BackendAdapter>,
    object: ObjectRef,
) -> Result<ImageMetadata, BackendError> {
    if !backend.capabilities().can_range_read {
        return Ok(ImageMetadata::default());
    }

    let Some(len) = backend.stat(&object).await?.size else {
        return Ok(ImageMetadata::default());
    };

    let runtime_handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        extract_image_metadata_remote_blocking(backend, object, len, runtime_handle)
    })
    .await
    .map_err(join_to_backend_error)
}

fn extract_image_metadata_remote_blocking(
    backend: Arc<dyn BackendAdapter>,
    object: ObjectRef,
    len: u64,
    runtime_handle: tokio::runtime::Handle,
) -> ImageMetadata {
    let source = Arc::new(RemoteWindowSource::new(
        backend,
        object.clone(),
        len,
        runtime_handle,
    ));
    let mut metadata = if is_x3f_path(Path::new(&object.display_path)) {
        extract_x3f_header_metadata(&source)
    } else {
        ImageMetadata::default()
    };

    if let Some((width, height)) = probe_image_dimensions(RemoteRangeReader::new(source.clone())) {
        if metadata.width.is_none() {
            metadata.width = Some(width);
        }
        if metadata.height.is_none() {
            metadata.height = Some(height);
        }
    }

    let extracted = extract_filtered_exif_metadata(RemoteRangeReader::new(source));
    if metadata.width.is_none() {
        metadata.width = extracted.width;
    }
    if metadata.height.is_none() {
        metadata.height = extracted.height;
    }
    metadata.media = extracted.media;
    if metadata.media.width.is_none() {
        metadata.media.width = metadata.width;
    }
    if metadata.media.height.is_none() {
        metadata.media.height = metadata.height;
    }
    merge_metadata_entries(&mut metadata.exif_entries, extracted.entries);

    metadata
}

fn probe_image_dimensions<R: Read + Seek>(reader: R) -> Option<(u32, u32)> {
    let reader = image::ImageReader::new(std::io::BufReader::new(reader))
        .with_guessed_format()
        .ok()?;
    reader.into_dimensions().ok()
}

fn extract_filtered_exif_metadata<R: Read + Seek>(reader: R) -> ExtractedExifMetadata {
    let mut reader = std::io::BufReader::new(reader);
    let Ok(exif_data) = exif::Reader::new().read_from_container(&mut reader) else {
        return ExtractedExifMetadata::default();
    };

    let mut width = None;
    let mut height = None;
    let mut media = MediaMetadata::default();
    let mut gps_lat_ref = None;
    let mut gps_lon_ref = None;
    let mut gps_lat = None;
    let mut gps_lon = None;

    for field in exif_data.fields() {
        capture_typed_exif_field(
            &mut media,
            &mut gps_lat_ref,
            &mut gps_lon_ref,
            &mut gps_lat,
            &mut gps_lon,
            field,
        );
        if field.tag == exif::Tag::PixelXDimension {
            width = field.value.get_uint(0);
        } else if field.tag == exif::Tag::PixelYDimension {
            height = field.value.get_uint(0);
        } else if field.tag == exif::Tag::ImageWidth && width.is_none() {
            width = field.value.get_uint(0);
        } else if field.tag == exif::Tag::ImageLength && height.is_none() {
            height = field.value.get_uint(0);
        }
    }
    media.width = width;
    media.height = height;
    media.gps_lat = gps_lat.map(|value| apply_gps_ref(value, gps_lat_ref.as_deref()));
    media.gps_lon = gps_lon.map(|value| apply_gps_ref(value, gps_lon_ref.as_deref()));

    let mut entries = Vec::new();
    let mut seen_tags = HashSet::new();
    for field in exif_data.fields() {
        let tag_name = format!("{}", field.tag);
        let value = field.display_value().with_unit(&exif_data).to_string();
        if value.len() < 500
            && !is_effectively_blank_exif_value(&value)
            && !tag_name.contains("Unknown")
            && !tag_name.starts_with("Tag(")
            && !should_skip_exif_tag(field.tag)
            && seen_tags.insert(tag_name.clone())
        {
            entries.push((tag_name, value));
        }
    }

    ExtractedExifMetadata {
        width,
        height,
        media,
        entries,
    }
}

fn capture_typed_exif_field(
    media: &mut MediaMetadata,
    gps_lat_ref: &mut Option<String>,
    gps_lon_ref: &mut Option<String>,
    gps_lat: &mut Option<f64>,
    gps_lon: &mut Option<f64>,
    field: &exif::Field,
) {
    match field.tag {
        exif::Tag::DateTimeOriginal | exif::Tag::DateTime => {
            if media.capture_naive_secs.is_none() {
                media.capture_naive_secs = ascii_value(&field.value).and_then(parse_exif_datetime);
            }
        }
        exif::Tag::OffsetTimeOriginal | exif::Tag::OffsetTime => {
            if media.capture_offset_min.is_none() {
                media.capture_offset_min = ascii_value(&field.value).and_then(parse_offset_minutes);
            }
        }
        exif::Tag::Make => {
            if media.camera_make.is_none() {
                media.camera_make = ascii_value(&field.value);
            }
        }
        exif::Tag::Model => {
            if media.camera_model.is_none() {
                media.camera_model = ascii_value(&field.value);
            }
        }
        exif::Tag::LensModel => {
            if media.lens_model.is_none() {
                media.lens_model = ascii_value(&field.value);
            }
        }
        exif::Tag::GPSLatitudeRef => {
            *gps_lat_ref = ascii_value(&field.value);
        }
        exif::Tag::GPSLongitudeRef => {
            *gps_lon_ref = ascii_value(&field.value);
        }
        exif::Tag::GPSLatitude => {
            *gps_lat = gps_dms_value(&field.value);
        }
        exif::Tag::GPSLongitude => {
            *gps_lon = gps_dms_value(&field.value);
        }
        _ => {}
    }
}

fn ascii_value(value: &exif::Value) -> Option<String> {
    let exif::Value::Ascii(values) = value else {
        return None;
    };
    values.iter().find_map(|bytes| {
        let text = String::from_utf8_lossy(bytes)
            .trim_matches(char::from(0))
            .trim()
            .to_string();
        (!text.is_empty()).then_some(text)
    })
}

fn parse_exif_datetime(value: String) -> Option<i64> {
    NaiveDateTime::parse_from_str(value.trim(), "%Y:%m:%d %H:%M:%S")
        .ok()
        .map(|dt| dt.and_utc().timestamp())
}

fn parse_offset_minutes(value: String) -> Option<i16> {
    let value = value.trim();
    if value.len() < 6 {
        return None;
    }
    let sign = match &value[0..1] {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let hours: i16 = value[1..3].parse().ok()?;
    let minutes: i16 = value[4..6].parse().ok()?;
    Some(sign * (hours * 60 + minutes))
}

fn gps_dms_value(value: &exif::Value) -> Option<f64> {
    let exif::Value::Rational(values) = value else {
        return None;
    };
    let deg = rational_to_f64(values.first()?)?;
    let min = rational_to_f64(values.get(1)?)?;
    let sec = rational_to_f64(values.get(2)?)?;
    Some(deg + min / 60.0 + sec / 3600.0)
}

fn rational_to_f64(value: &exif::Rational) -> Option<f64> {
    (value.denom != 0).then_some(value.num as f64 / value.denom as f64)
}

fn apply_gps_ref(value: f64, reference: Option<&str>) -> f64 {
    match reference {
        Some(reference)
            if reference.eq_ignore_ascii_case("S") || reference.eq_ignore_ascii_case("W") =>
        {
            -value
        }
        _ => value,
    }
}

pub(crate) fn media_metadata_from_display_entries(
    width: Option<u32>,
    height: Option<u32>,
    entries: &[(String, String)],
) -> MediaMetadata {
    let mut media = MediaMetadata {
        width,
        height,
        ..MediaMetadata::default()
    };
    for (tag, value) in entries {
        match tag.as_str() {
            "Make" if media.camera_make.is_none() => media.camera_make = Some(value.clone()),
            "Model" if media.camera_model.is_none() => media.camera_model = Some(value.clone()),
            "Lens Model" | "LensModel" if media.lens_model.is_none() => {
                media.lens_model = Some(value.clone());
            }
            _ => {}
        }
    }
    media
}

fn should_skip_exif_tag(tag: exif::Tag) -> bool {
    matches!(
        tag,
        exif::Tag::JPEGInterchangeFormat
            | exif::Tag::JPEGInterchangeFormatLength
            | exif::Tag::StripOffsets
            | exif::Tag::StripByteCounts
    )
}

fn is_effectively_blank_exif_value(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_whitespace() || matches!(ch, '"' | ',' | '\0'))
}

fn merge_metadata_entries(entries: &mut Vec<(String, String)>, additional: Vec<(String, String)>) {
    let mut seen: HashSet<String> = entries.iter().map(|(tag, _)| tag.clone()).collect();
    for (tag, value) in additional {
        if value.len() >= 500 || !seen.insert(tag.clone()) {
            continue;
        }
        entries.push((tag, value));
    }
}

fn is_raw_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(is_known_raw_extension)
        .unwrap_or(false)
}

fn is_x3f_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| supports_raw_extension(ext) && ext.eq_ignore_ascii_case("x3f"))
}

fn extract_x3f_header_metadata(source: &RemoteWindowSource) -> ImageMetadata {
    let mut metadata = ImageMetadata::default();
    if source.len() < X3F_ROTATION_OFFSET + 4 {
        return metadata;
    }

    let Ok(signature) = source.read_range(0, 4) else {
        return metadata;
    };
    if signature.as_slice() != X3F_SIGNATURE {
        return metadata;
    }

    let Ok(file_version) = source.read_range(X3F_FILE_VERSION_OFFSET, 4) else {
        return metadata;
    };
    let Ok(width) = source.read_range(X3F_IMAGE_WIDTH_OFFSET, 4) else {
        return metadata;
    };
    let Ok(height) = source.read_range(X3F_IMAGE_HEIGHT_OFFSET, 4) else {
        return metadata;
    };
    let Ok(rotation) = source.read_range(X3F_ROTATION_OFFSET, 4) else {
        return metadata;
    };

    metadata.width = read_u32_le(&width).filter(|value| *value > 0);
    metadata.height = read_u32_le(&height).filter(|value| *value > 0);
    if let Some(version) = read_u32_le(&file_version) {
        metadata
            .exif_entries
            .push(("File Version".to_string(), format_x3f_file_version(version)));
    }
    if let Some(rotation) = read_u32_le(&rotation) {
        metadata
            .exif_entries
            .push(("Rotation".to_string(), rotation.to_string()));
    }

    metadata
}

fn format_x3f_file_version(raw: u32) -> String {
    format!("{}.{}", raw >> 16, raw & 0xFFFF)
}

fn read_u32_le(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests;
