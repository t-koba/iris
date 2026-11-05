use super::*;

pub(super) fn extract_tiff_primary_dimensions(source: &mut SourceFile) -> Option<(u32, u32)> {
    if source.len() < 8 {
        return None;
    }

    let header = source.read_range(0, source.len().min(16) as usize)?;
    let endian = parse_tiff_endian(&header)?;
    let magic = read_u16_endian(&header[2..4], endian)?;
    if magic == TIFF_MAGIC_BIGTIFF {
        return extract_bigtiff_primary_dimensions(source, endian, &header);
    }
    if !matches!(magic, TIFF_MAGIC_STANDARD | TIFF_MAGIC_RW2) {
        return None;
    }

    let first_ifd = read_u32_endian(&header[4..8], endian)? as u64;
    if first_ifd == 0 || first_ifd >= source.len() {
        return None;
    }

    let mut stack = vec![first_ifd];
    let mut visited = HashSet::new();
    let mut candidates = Vec::new();

    while let Some(ifd_offset) = stack.pop() {
        if ifd_offset == 0 || ifd_offset + 2 > source.len() {
            continue;
        }
        if !visited.insert(ifd_offset) {
            continue;
        }
        if visited.len() > MAX_IFD_VISITS {
            break;
        }

        let count_bytes = source.read_range(ifd_offset, 2)?;
        let entry_count = read_u16_endian(&count_bytes, endian)? as usize;
        if entry_count == 0 || entry_count > MAX_IFD_ENTRIES {
            continue;
        }

        let table_len = 2usize
            .checked_add(entry_count.checked_mul(12)?)
            .and_then(|value| value.checked_add(4))?;
        if ifd_offset + table_len as u64 > source.len() {
            continue;
        }

        let table = source.read_range(ifd_offset, table_len)?;
        let mut width = None;
        let mut height = None;
        let mut new_subfile_type = None;
        let mut subfile_type = None;
        let mut sensor_width = None;
        let mut sensor_height = None;
        let mut sensor_top_border = None;
        let mut sensor_left_border = None;
        let mut sensor_bottom_border = None;
        let mut sensor_right_border = None;

        for idx in 0..entry_count {
            let base = 2 + idx * 12;
            let tag = read_u16_endian(&table[base..base + 2], endian)?;
            let field_type = read_u16_endian(&table[base + 2..base + 4], endian)?;
            let count = read_u32_endian(&table[base + 4..base + 8], endian)?;
            let value_or_offset = read_u32_endian(&table[base + 8..base + 12], endian)?;

            match tag {
                TAG_IMAGE_WIDTH => {
                    width = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_IMAGE_LENGTH => {
                    height = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_PANASONIC_SENSOR_WIDTH => {
                    sensor_width = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_PANASONIC_SENSOR_HEIGHT => {
                    sensor_height = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_PANASONIC_SENSOR_TOP_BORDER => {
                    sensor_top_border = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_PANASONIC_SENSOR_LEFT_BORDER => {
                    sensor_left_border = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_PANASONIC_SENSOR_BOTTOM_BORDER => {
                    sensor_bottom_border = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_PANASONIC_SENSOR_RIGHT_BORDER => {
                    sensor_right_border = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_NEW_SUBFILE_TYPE => {
                    new_subfile_type = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_SUBFILE_TYPE => {
                    subfile_type = read_first_u32_tag_value(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_SUB_IFDS => {
                    for off in
                        read_u32_tag_array(source, endian, field_type, count, value_or_offset)
                    {
                        if off > 0 {
                            stack.push(off as u64);
                        }
                    }
                }
                _ => {}
            }
        }

        if width.is_none() {
            width =
                panasonic_image_dimension(sensor_left_border, sensor_right_border).or(sensor_width);
        }
        if height.is_none() {
            height = panasonic_image_dimension(sensor_top_border, sensor_bottom_border)
                .or(sensor_height);
        }

        if let (Some(width), Some(height)) = (width, height) {
            if width > 0 && height > 0 {
                candidates.push(PrimaryImageDimensionsCandidate {
                    width,
                    height,
                    reduced_resolution: is_reduced_resolution_ifd(new_subfile_type, subfile_type),
                });
            }
        }

        let next_base = 2 + entry_count * 12;
        let next_ifd = read_u32_endian(&table[next_base..next_base + 4], endian)? as u64;
        if next_ifd > 0 {
            stack.push(next_ifd);
        }
    }

    choose_primary_dimensions(candidates)
}

pub(super) fn extract_bigtiff_primary_dimensions(
    source: &mut SourceFile,
    endian: Endian,
    header: &[u8],
) -> Option<(u32, u32)> {
    if header.len() < 16 {
        return None;
    }
    if read_u16_endian(&header[4..6], endian)? != BIGTIFF_OFFSET_SIZE {
        return None;
    }
    if read_u16_endian(&header[6..8], endian)? != 0 {
        return None;
    }

    let first_ifd = read_u64_endian(&header[8..16], endian)?;
    if first_ifd == 0 || first_ifd >= source.len() {
        return None;
    }

    let mut stack = vec![first_ifd];
    let mut visited = HashSet::new();
    let mut candidates = Vec::new();

    while let Some(ifd_offset) = stack.pop() {
        if ifd_offset == 0 || ifd_offset + 8 > source.len() {
            continue;
        }
        if !visited.insert(ifd_offset) {
            continue;
        }
        if visited.len() > MAX_IFD_VISITS {
            break;
        }

        let count_bytes = source.read_range(ifd_offset, 8)?;
        let entry_count = usize::try_from(read_u64_endian(&count_bytes, endian)?).ok()?;
        if entry_count == 0 || entry_count > MAX_IFD_ENTRIES {
            continue;
        }

        let table_len = 8usize
            .checked_add(entry_count.checked_mul(BIGTIFF_ENTRY_BYTES)?)
            .and_then(|value| value.checked_add(8))?;
        if ifd_offset + table_len as u64 > source.len() {
            continue;
        }

        let table = source.read_range(ifd_offset, table_len)?;
        let mut width = None;
        let mut height = None;
        let mut new_subfile_type = None;
        let mut subfile_type = None;

        for idx in 0..entry_count {
            let base = 8 + idx * BIGTIFF_ENTRY_BYTES;
            let tag = read_u16_endian(&table[base..base + 2], endian)?;
            let field_type = read_u16_endian(&table[base + 2..base + 4], endian)?;
            let count = read_u64_endian(&table[base + 4..base + 12], endian)?;
            let value_or_offset = read_u64_endian(&table[base + 12..base + 20], endian)?;

            match tag {
                TAG_IMAGE_WIDTH => {
                    width = read_first_u64_tag_value_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    )
                    .and_then(|value| u32::try_from(value).ok());
                }
                TAG_IMAGE_LENGTH => {
                    height = read_first_u64_tag_value_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    )
                    .and_then(|value| u32::try_from(value).ok());
                }
                TAG_NEW_SUBFILE_TYPE => {
                    new_subfile_type = read_first_u64_tag_value_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    )
                    .and_then(|value| u32::try_from(value).ok());
                }
                TAG_SUBFILE_TYPE => {
                    subfile_type = read_first_u64_tag_value_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    )
                    .and_then(|value| u32::try_from(value).ok());
                }
                TAG_SUB_IFDS => {
                    for off in read_u64_tag_array_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    ) {
                        if off > 0 {
                            stack.push(off);
                        }
                    }
                }
                _ => {}
            }
        }

        if let (Some(width), Some(height)) = (width, height) {
            if width > 0 && height > 0 {
                candidates.push(PrimaryImageDimensionsCandidate {
                    width,
                    height,
                    reduced_resolution: is_reduced_resolution_ifd(new_subfile_type, subfile_type),
                });
            }
        }

        let next_base = 8 + entry_count * BIGTIFF_ENTRY_BYTES;
        let next_ifd = read_u64_endian(&table[next_base..next_base + 8], endian)?;
        if next_ifd > 0 {
            stack.push(next_ifd);
        }
    }

    choose_primary_dimensions(candidates)
}
