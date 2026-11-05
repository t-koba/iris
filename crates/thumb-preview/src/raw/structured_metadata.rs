use super::*;

pub(super) fn extract_tiff_structured_metadata(
    source: &mut SourceFile,
) -> Option<StructuredRawMetadata> {
    let dimensions = extract_tiff_primary_dimensions(source);
    let mut metadata = StructuredRawMetadata {
        width: dimensions.map(|(width, _)| width),
        height: dimensions.map(|(_, height)| height),
        entries: Vec::new(),
    };

    if source.len() < 8 {
        return Some(metadata);
    }

    let header = source.read_range(0, source.len().min(16) as usize)?;
    let endian = parse_tiff_endian(&header)?;
    let magic = read_u16_endian(&header[2..4], endian)?;
    if magic == TIFF_MAGIC_BIGTIFF {
        let first_ifd = read_u64_endian(&header[8..16], endian)?;
        metadata.entries = extract_bigtiff_root_entries(source, endian, first_ifd);
        return Some(metadata);
    }
    if !matches!(magic, TIFF_MAGIC_STANDARD | TIFF_MAGIC_RW2) {
        return Some(metadata);
    }

    let first_ifd = read_u32_endian(&header[4..8], endian)? as u64;
    metadata.entries = extract_tiff_root_entries(source, endian, first_ifd);
    Some(metadata)
}

#[cfg(feature = "raw-full")]
pub(super) fn extract_x3f_structured_metadata(
    source: &mut SourceFile,
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

pub(super) fn merge_metadata_entries(
    entries: &mut Vec<(String, String)>,
    additional: Vec<(String, String)>,
) {
    let mut seen: HashSet<String> = entries.iter().map(|(tag, _)| tag.clone()).collect();
    for (tag, value) in additional {
        if value.len() >= 500 || !seen.insert(tag.clone()) {
            continue;
        }
        entries.push((tag, value));
    }
}

pub(super) fn push_metadata_entry(entries: &mut Vec<(String, String)>, tag: &str, value: String) {
    if value.is_empty() || is_effectively_blank_exif_value(&value) {
        return;
    }
    if entries.iter().any(|(existing, _)| existing == tag) {
        return;
    }
    entries.push((tag.to_string(), value));
}

pub(super) fn extract_tiff_root_entries(
    source: &mut SourceFile,
    endian: Endian,
    ifd_offset: u64,
) -> Vec<(String, String)> {
    if ifd_offset == 0 || ifd_offset + 2 > source.len() {
        return Vec::new();
    }

    let Some(count_bytes) = source.read_range(ifd_offset, 2) else {
        return Vec::new();
    };
    let Some(entry_count) = read_u16_endian(&count_bytes, endian).map(|v| v as usize) else {
        return Vec::new();
    };
    if entry_count == 0 || entry_count > MAX_IFD_ENTRIES {
        return Vec::new();
    }

    let Some(table_len) = 2usize
        .checked_add(entry_count.checked_mul(12).unwrap_or(0))
        .and_then(|value| value.checked_add(4))
    else {
        return Vec::new();
    };
    if ifd_offset + table_len as u64 > source.len() {
        return Vec::new();
    }

    let Some(table) = source.read_range(ifd_offset, table_len) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    let mut sensor_width = None;
    let mut sensor_height = None;
    let mut sensor_top_border = None;
    let mut sensor_left_border = None;
    let mut sensor_bottom_border = None;
    let mut sensor_right_border = None;

    for idx in 0..entry_count {
        let base = 2 + idx * 12;
        let Some(tag) = read_u16_endian(&table[base..base + 2], endian) else {
            continue;
        };
        let Some(field_type) = read_u16_endian(&table[base + 2..base + 4], endian) else {
            continue;
        };
        let Some(count) = read_u32_endian(&table[base + 4..base + 8], endian) else {
            continue;
        };
        let Some(value_or_offset) = read_u32_endian(&table[base + 8..base + 12], endian) else {
            continue;
        };

        match tag {
            TAG_BITS_PER_SAMPLE | TAG_PANASONIC_BITS_PER_SAMPLE => {
                let values = read_u32_tag_array(source, endian, field_type, count, value_or_offset);
                if let Some(value) = format_bits_per_sample_values(&values) {
                    push_metadata_entry(&mut entries, "Bits Per Sample", value);
                }
            }
            TAG_MAKE => {
                if let Some(value) =
                    read_tiff_ascii_tag_value(source, endian, field_type, count, value_or_offset)
                {
                    push_metadata_entry(&mut entries, "Make", value);
                }
            }
            TAG_MODEL => {
                if let Some(value) =
                    read_tiff_ascii_tag_value(source, endian, field_type, count, value_or_offset)
                {
                    push_metadata_entry(&mut entries, "Model", value);
                }
            }
            TAG_ORIENTATION => {
                if let Some(value) =
                    read_first_u32_tag_value(source, endian, field_type, count, value_or_offset)
                {
                    push_metadata_entry(&mut entries, "Orientation", format_orientation(value));
                }
            }
            TAG_PANASONIC_RAW_VERSION => {
                if let Some(value) =
                    read_tiff_ascii_tag_value(source, endian, field_type, count, value_or_offset)
                {
                    push_metadata_entry(&mut entries, "Panasonic Raw Version", value);
                }
            }
            TAG_PANASONIC_SENSOR_WIDTH => {
                sensor_width =
                    read_first_u32_tag_value(source, endian, field_type, count, value_or_offset);
            }
            TAG_PANASONIC_SENSOR_HEIGHT => {
                sensor_height =
                    read_first_u32_tag_value(source, endian, field_type, count, value_or_offset);
            }
            TAG_PANASONIC_SENSOR_TOP_BORDER => {
                sensor_top_border =
                    read_first_u32_tag_value(source, endian, field_type, count, value_or_offset);
            }
            TAG_PANASONIC_SENSOR_LEFT_BORDER => {
                sensor_left_border =
                    read_first_u32_tag_value(source, endian, field_type, count, value_or_offset);
            }
            TAG_PANASONIC_SENSOR_BOTTOM_BORDER => {
                sensor_bottom_border =
                    read_first_u32_tag_value(source, endian, field_type, count, value_or_offset);
            }
            TAG_PANASONIC_SENSOR_RIGHT_BORDER => {
                sensor_right_border =
                    read_first_u32_tag_value(source, endian, field_type, count, value_or_offset);
            }
            TAG_PANASONIC_RAW_FORMAT => {
                if let Some(value) =
                    read_first_u32_tag_value(source, endian, field_type, count, value_or_offset)
                {
                    push_metadata_entry(&mut entries, "Raw Format", value.to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(value) = sensor_width {
        push_metadata_entry(&mut entries, "Sensor Width", value.to_string());
    }
    if let Some(value) = sensor_height {
        push_metadata_entry(&mut entries, "Sensor Height", value.to_string());
    }
    if let Some(value) = sensor_top_border {
        push_metadata_entry(&mut entries, "Sensor Top Border", value.to_string());
    }
    if let Some(value) = sensor_left_border {
        push_metadata_entry(&mut entries, "Sensor Left Border", value.to_string());
    }
    if let Some(value) = sensor_bottom_border {
        push_metadata_entry(&mut entries, "Sensor Bottom Border", value.to_string());
    }
    if let Some(value) = sensor_right_border {
        push_metadata_entry(&mut entries, "Sensor Right Border", value.to_string());
    }

    entries
}

pub(super) fn extract_bigtiff_root_entries(
    source: &mut SourceFile,
    endian: Endian,
    ifd_offset: u64,
) -> Vec<(String, String)> {
    if ifd_offset == 0 || ifd_offset + 8 > source.len() {
        return Vec::new();
    }

    let Some(count_bytes) = source.read_range(ifd_offset, 8) else {
        return Vec::new();
    };
    let Some(entry_count) =
        read_u64_endian(&count_bytes, endian).and_then(|v| usize::try_from(v).ok())
    else {
        return Vec::new();
    };
    if entry_count == 0 || entry_count > MAX_IFD_ENTRIES {
        return Vec::new();
    }

    let Some(table_len) = 8usize
        .checked_add(entry_count.checked_mul(BIGTIFF_ENTRY_BYTES).unwrap_or(0))
        .and_then(|value| value.checked_add(8))
    else {
        return Vec::new();
    };
    if ifd_offset + table_len as u64 > source.len() {
        return Vec::new();
    }

    let Some(table) = source.read_range(ifd_offset, table_len) else {
        return Vec::new();
    };
    let mut entries = Vec::new();

    for idx in 0..entry_count {
        let base = 8 + idx * BIGTIFF_ENTRY_BYTES;
        let Some(tag) = read_u16_endian(&table[base..base + 2], endian) else {
            continue;
        };
        let Some(field_type) = read_u16_endian(&table[base + 2..base + 4], endian) else {
            continue;
        };
        let Some(count) = read_u64_endian(&table[base + 4..base + 12], endian) else {
            continue;
        };
        let Some(value_or_offset) = read_u64_endian(&table[base + 12..base + 20], endian) else {
            continue;
        };

        match tag {
            TAG_BITS_PER_SAMPLE => {
                let values =
                    read_u64_tag_array_bigtiff(source, endian, field_type, count, value_or_offset);
                if let Some(value) = format_bits_per_sample_values_u64(&values) {
                    push_metadata_entry(&mut entries, "Bits Per Sample", value);
                }
            }
            TAG_MAKE => {
                if let Some(value) = read_tiff_ascii_tag_value_bigtiff(
                    source,
                    endian,
                    field_type,
                    count,
                    value_or_offset,
                ) {
                    push_metadata_entry(&mut entries, "Make", value);
                }
            }
            TAG_MODEL => {
                if let Some(value) = read_tiff_ascii_tag_value_bigtiff(
                    source,
                    endian,
                    field_type,
                    count,
                    value_or_offset,
                ) {
                    push_metadata_entry(&mut entries, "Model", value);
                }
            }
            TAG_ORIENTATION => {
                if let Some(value) = read_first_u64_tag_value_bigtiff(
                    source,
                    endian,
                    field_type,
                    count,
                    value_or_offset,
                ) {
                    push_metadata_entry(
                        &mut entries,
                        "Orientation",
                        format_orientation(value as u32),
                    );
                }
            }
            _ => {}
        }
    }

    entries
}

pub(super) fn format_bits_per_sample_values(values: &[u32]) -> Option<String> {
    format_bits_per_sample_iter(values.iter().copied())
}

pub(super) fn format_bits_per_sample_values_u64(values: &[u64]) -> Option<String> {
    format_bits_per_sample_iter(values.iter().filter_map(|value| u32::try_from(*value).ok()))
}

pub(super) fn format_bits_per_sample_iter<I>(values: I) -> Option<String>
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
