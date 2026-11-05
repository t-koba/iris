use super::*;

pub(super) fn extract_tiff_embedded_jpeg(
    source: &impl BlockingBinaryReader,
    max_dimension: u32,
) -> Option<StructuredJpegCandidate> {
    if source.len() < 8 {
        return None;
    }

    let header = source.read_range(0, source.len().min(16) as usize)?;
    let endian = parse_tiff_endian(&header)?;
    let magic = read_u16_endian(&header[2..4], endian)?;
    if magic == TIFF_MAGIC_BIGTIFF {
        return extract_bigtiff_embedded_jpeg(source, endian, &header, max_dimension);
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

        let count_bytes = match source.read_range(ifd_offset, 2) {
            Some(v) => v,
            None => continue,
        };
        let entry_count = read_u16_endian(&count_bytes, endian)? as usize;
        if entry_count == 0 || entry_count > MAX_IFD_ENTRIES {
            continue;
        }

        let table_len = 2usize
            .checked_add(entry_count.checked_mul(12)?)
            .and_then(|v| v.checked_add(4))?;
        if ifd_offset + table_len as u64 > source.len() {
            continue;
        }

        let table = match source.read_range(ifd_offset, table_len) {
            Some(v) => v,
            None => continue,
        };

        let mut jpeg_offset = None;
        let mut jpeg_length = None;
        let mut compression = None;
        let mut strip_offsets = Vec::new();
        let mut strip_byte_counts = Vec::new();
        let mut tile_offsets = Vec::new();
        let mut tile_byte_counts = Vec::new();

        for idx in 0..entry_count {
            let base = 2 + idx * 12;
            let tag = read_u16_endian(&table[base..base + 2], endian)?;
            let field_type = read_u16_endian(&table[base + 2..base + 4], endian)?;
            let count = read_u32_endian(&table[base + 4..base + 8], endian)?;
            let value_or_offset = read_u32_endian(&table[base + 8..base + 12], endian)?;

            match tag {
                TAG_COMPRESSION => {
                    if let Some(v) =
                        read_first_u32_tag_value(source, endian, field_type, count, value_or_offset)
                    {
                        compression = Some(v as u16);
                    }
                }
                TAG_STRIP_OFFSETS => {
                    strip_offsets =
                        read_u32_tag_array(source, endian, field_type, count, value_or_offset);
                }
                TAG_STRIP_BYTE_COUNTS => {
                    strip_byte_counts =
                        read_u32_tag_array(source, endian, field_type, count, value_or_offset);
                }
                TAG_TILE_OFFSETS => {
                    tile_offsets =
                        read_u32_tag_array(source, endian, field_type, count, value_or_offset);
                }
                TAG_TILE_BYTE_COUNTS => {
                    tile_byte_counts =
                        read_u32_tag_array(source, endian, field_type, count, value_or_offset);
                }
                TAG_JPEG_INTERCHANGE_FORMAT => {
                    if let Some(v) =
                        read_first_u32_tag_value(source, endian, field_type, count, value_or_offset)
                    {
                        jpeg_offset = Some(v as u64);
                    }
                }
                TAG_JPEG_INTERCHANGE_FORMAT_LENGTH => {
                    if let Some(v) =
                        read_first_u32_tag_value(source, endian, field_type, count, value_or_offset)
                    {
                        jpeg_length = Some(v as u64);
                    }
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
                TAG_EXIF_IFD_POINTER => {
                    if let Some(v) =
                        read_first_u32_tag_value(source, endian, field_type, count, value_or_offset)
                    {
                        if v > 0 {
                            stack.push(v as u64);
                        }
                    }
                }
                TAG_PANASONIC_JPG_FROM_RAW | TAG_PANASONIC_JPG_FROM_RAW2 => {
                    if let Some(candidate) =
                        tiff_tag_candidate(source, endian, field_type, count, value_or_offset)
                    {
                        candidates.push(candidate);
                    }
                }
                _ => {}
            }
        }

        if let (Some(start), Some(length)) = (jpeg_offset, jpeg_length) {
            if let Some(candidate) = candidate_from_range(source, start, length as usize) {
                candidates.push(candidate);
            }
        }

        let is_jpeg_compressed =
            compression.is_some_and(|c| c == COMPRESSION_JPEG_OLD || c == COMPRESSION_JPEG_NEW);
        if is_jpeg_compressed {
            if let Some(candidate) =
                candidate_from_segments(source, &strip_offsets, &strip_byte_counts)
            {
                candidates.push(candidate);
            }
            if let Some(candidate) =
                candidate_from_segments(source, &tile_offsets, &tile_byte_counts)
            {
                candidates.push(candidate);
            }
        }

        let next_base = 2 + entry_count * 12;
        let next_ifd = read_u32_endian(&table[next_base..next_base + 4], endian)? as u64;
        if next_ifd > 0 {
            stack.push(next_ifd);
        }
    }

    choose_best_candidate(candidates, max_dimension)
}

pub(super) fn extract_bigtiff_embedded_jpeg(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    header: &[u8],
    max_dimension: u32,
) -> Option<StructuredJpegCandidate> {
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
            .and_then(|v| v.checked_add(8))?;
        if ifd_offset + table_len as u64 > source.len() {
            continue;
        }

        let table = source.read_range(ifd_offset, table_len)?;
        let mut jpeg_offset = None;
        let mut jpeg_length = None;
        let mut compression = None;
        let mut strip_offsets = Vec::new();
        let mut strip_byte_counts = Vec::new();
        let mut tile_offsets = Vec::new();
        let mut tile_byte_counts = Vec::new();

        for idx in 0..entry_count {
            let base = 8 + idx * BIGTIFF_ENTRY_BYTES;
            let tag = read_u16_endian(&table[base..base + 2], endian)?;
            let field_type = read_u16_endian(&table[base + 2..base + 4], endian)?;
            let count = read_u64_endian(&table[base + 4..base + 12], endian)?;
            let value_or_offset = read_u64_endian(&table[base + 12..base + 20], endian)?;

            match tag {
                TAG_COMPRESSION => {
                    compression = read_first_u64_tag_value_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    )
                    .map(|v| v as u16);
                }
                TAG_STRIP_OFFSETS => {
                    strip_offsets = read_u64_tag_array_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_STRIP_BYTE_COUNTS => {
                    strip_byte_counts = read_u64_tag_array_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_TILE_OFFSETS => {
                    tile_offsets = read_u64_tag_array_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_TILE_BYTE_COUNTS => {
                    tile_byte_counts = read_u64_tag_array_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_JPEG_INTERCHANGE_FORMAT => {
                    jpeg_offset = read_first_u64_tag_value_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
                }
                TAG_JPEG_INTERCHANGE_FORMAT_LENGTH => {
                    jpeg_length = read_first_u64_tag_value_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    );
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
                TAG_EXIF_IFD_POINTER => {
                    if let Some(v) = read_first_u64_tag_value_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    ) {
                        if v > 0 {
                            stack.push(v);
                        }
                    }
                }
                TAG_PANASONIC_JPG_FROM_RAW | TAG_PANASONIC_JPG_FROM_RAW2 => {
                    if let Some(candidate) = tiff_tag_candidate_bigtiff(
                        source,
                        endian,
                        field_type,
                        count,
                        value_or_offset,
                    ) {
                        candidates.push(candidate);
                    }
                }
                _ => {}
            }
        }

        if let (Some(start), Some(length)) = (jpeg_offset, jpeg_length) {
            if let Ok(len) = usize::try_from(length) {
                if let Some(candidate) = candidate_from_range(source, start, len) {
                    candidates.push(candidate);
                }
            }
        }

        let is_jpeg_compressed =
            compression.is_some_and(|c| c == COMPRESSION_JPEG_OLD || c == COMPRESSION_JPEG_NEW);
        if is_jpeg_compressed {
            if let Some(candidate) =
                candidate_from_segments_u64(source, &strip_offsets, &strip_byte_counts)
            {
                candidates.push(candidate);
            }
            if let Some(candidate) =
                candidate_from_segments_u64(source, &tile_offsets, &tile_byte_counts)
            {
                candidates.push(candidate);
            }
        }

        let next_base = 8 + entry_count * BIGTIFF_ENTRY_BYTES;
        let next_ifd = read_u64_endian(&table[next_base..next_base + 8], endian)?;
        if next_ifd > 0 {
            stack.push(next_ifd);
        }
    }

    choose_best_candidate(candidates, max_dimension)
}
