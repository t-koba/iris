use super::*;

pub(super) fn extract_tiff_structured_metadata(
    source: &impl BlockingBinaryReader,
) -> Option<StructuredRawMetadata> {
    let dimensions = extract_tiff_primary_dimensions(source);
    Some(StructuredRawMetadata {
        width: dimensions.map(|(width, _)| width),
        height: dimensions.map(|(_, height)| height),
        entries: Tiff::parse(source)
            .map(|tiff| tiff_root_entries(tiff, source))
            .unwrap_or_default(),
    })
}

#[cfg(feature = "raw-full")]
pub(super) fn extract_x3f_structured_metadata(
    source: &impl BlockingBinaryReader,
) -> Option<StructuredRawMetadata> {
    if source.len() < X3F_ROTATION_OFFSET + 4 {
        return None;
    }

    let signature = source.read_range(0, 4)?;
    if signature.as_slice() != X3F_SIGNATURE {
        return None;
    }

    let file_version = read_u32_le(&source.read_range(X3F_FILE_VERSION_OFFSET, 4)?)?;
    let mut width = read_u32_le(&source.read_range(X3F_IMAGE_WIDTH_OFFSET, 4)?)?;
    let mut height = read_u32_le(&source.read_range(X3F_IMAGE_HEIGHT_OFFSET, 4)?)?;
    let rotation = read_u32_le(&source.read_range(X3F_ROTATION_OFFSET, 4)?)?;
    if width == 0 || height == 0 {
        if let Some((candidate_width, candidate_height)) = extract_x3f_primary_dimensions(source) {
            width = candidate_width;
            height = candidate_height;
        }
    }

    let mut entries = Vec::new();
    push_metadata_entry(
        &mut entries,
        "File Version",
        format_x3f_file_version(file_version),
    );
    push_metadata_entry(&mut entries, "Rotation", rotation.to_string());

    Some(StructuredRawMetadata {
        width: (width > 0).then_some(width),
        height: (height > 0).then_some(height),
        entries,
    })
}

pub(super) fn push_metadata_entry(entries: &mut Vec<(String, String)>, tag: &str, value: String) {
    if value.is_empty() || crate::metadata::is_effectively_blank_exif_value(&value) {
        return;
    }
    if entries.iter().any(|(existing, _)| existing == tag) {
        return;
    }
    entries.push((tag.to_string(), value));
}

fn tiff_root_entries(tiff: Tiff, source: &impl BlockingBinaryReader) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let root = tiff.root_entries(source);
    for entry in &root {
        let label = match entry.tag {
            TAG_MAKE => "Make",
            TAG_MODEL => "Model",
            TAG_PANASONIC_RAW_VERSION => "Panasonic Raw Version",
            TAG_BITS_PER_SAMPLE | TAG_PANASONIC_BITS_PER_SAMPLE => {
                let values = tiff.values(source, entry);
                let values = values.iter().filter_map(|value| u32::try_from(*value).ok());
                if let Some(value) = format_bits_per_sample_iter(values) {
                    push_metadata_entry(&mut entries, "Bits Per Sample", value);
                }
                continue;
            }
            TAG_ORIENTATION => {
                if let Some(value) = tiff.first_u32(source, entry) {
                    push_metadata_entry(&mut entries, "Orientation", format_orientation(value));
                }
                continue;
            }
            TAG_PANASONIC_RAW_FORMAT => {
                if let Some(value) = tiff.first_u32(source, entry) {
                    push_metadata_entry(&mut entries, "Raw Format", value.to_string());
                }
                continue;
            }
            _ => continue,
        };
        if let Some(value) = tiff.ascii(source, entry) {
            push_metadata_entry(&mut entries, label, value);
        }
    }
    for (tag, label) in [
        (TAG_PANASONIC_SENSOR_WIDTH, "Sensor Width"),
        (TAG_PANASONIC_SENSOR_HEIGHT, "Sensor Height"),
        (TAG_PANASONIC_SENSOR_TOP_BORDER, "Sensor Top Border"),
        (TAG_PANASONIC_SENSOR_LEFT_BORDER, "Sensor Left Border"),
        (TAG_PANASONIC_SENSOR_BOTTOM_BORDER, "Sensor Bottom Border"),
        (TAG_PANASONIC_SENSOR_RIGHT_BORDER, "Sensor Right Border"),
    ] {
        if let Some(value) = root
            .iter()
            .rfind(|entry| entry.tag == tag)
            .and_then(|entry| tiff.first_u32(source, entry))
        {
            push_metadata_entry(&mut entries, label, value.to_string());
        }
    }
    entries
}

fn format_bits_per_sample_iter<I>(values: I) -> Option<String>
where
    I: IntoIterator<Item = u32>,
{
    let values: Vec<u32> = values.into_iter().filter(|value| *value > 0).collect();
    if values.is_empty() {
        return None;
    }
    if values.windows(2).all(|window| window[0] == window[1]) {
        return Some(values[0].to_string());
    }
    Some(
        values
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

pub(super) fn format_orientation(value: u32) -> String {
    match value {
        1 => "Horizontal (normal)".to_string(),
        3 => "Rotate 180".to_string(),
        6 => "Rotate 90 CW".to_string(),
        8 => "Rotate 270 CW".to_string(),
        other => other.to_string(),
    }
}

#[cfg(feature = "raw-full")]
pub(super) fn format_x3f_file_version(raw: u32) -> String {
    format!("{}.{}", raw >> 16, raw & 0xFFFF)
}
