use super::*;

pub(super) struct SampleToChunkEntry {
    first_chunk: u32,
    samples_per_chunk: u32,
}

#[derive(Clone, Debug)]
pub(super) struct SampleSizeTable {
    constant_size: Option<usize>,
    sizes: Vec<usize>,
    sample_count: usize,
}

impl SampleSizeTable {
    fn len(&self) -> usize {
        self.sample_count
    }

    fn size_at(&self, index: usize) -> Option<usize> {
        if index >= self.sample_count {
            return None;
        }
        match self.constant_size {
            Some(size) => Some(size),
            None => self.sizes.get(index).copied(),
        }
    }
}

pub(super) fn stbl_sample_ranges(
    source: &impl BlockingBinaryReader,
    stbl: FileBoxHeader,
) -> Vec<(u64, usize)> {
    let Some(stsc) = find_box_in_range(source, stbl.data_start(), stbl.end(), BOX_TYPE_STSC) else {
        return Vec::new();
    };
    let Some(sample_to_chunk) = parse_stsc(source, stsc) else {
        return Vec::new();
    };
    let sample_sizes = find_box_in_range(source, stbl.data_start(), stbl.end(), BOX_TYPE_STSZ)
        .and_then(|stsz| parse_stsz(source, stsz))
        .or_else(|| {
            find_box_in_range(source, stbl.data_start(), stbl.end(), BOX_TYPE_STZ2)
                .and_then(|stz2| parse_stz2(source, stz2))
        });
    let Some(sample_sizes) = sample_sizes else {
        return Vec::new();
    };
    let chunk_offsets = find_box_in_range(source, stbl.data_start(), stbl.end(), BOX_TYPE_CO64)
        .and_then(|co64| parse_chunk_offsets(source, co64, 8))
        .or_else(|| {
            find_box_in_range(source, stbl.data_start(), stbl.end(), BOX_TYPE_STCO)
                .and_then(|stco| parse_chunk_offsets(source, stco, 4))
        })
        .unwrap_or_default();

    build_sample_ranges(&chunk_offsets, &sample_to_chunk, &sample_sizes)
}

pub(super) fn build_sample_ranges(
    chunk_offsets: &[u64],
    sample_to_chunk: &[SampleToChunkEntry],
    sample_sizes: &SampleSizeTable,
) -> Vec<(u64, usize)> {
    if chunk_offsets.is_empty() || sample_to_chunk.is_empty() || sample_sizes.len() == 0 {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut stsc_index = 0usize;
    let mut sample_index = 0usize;

    for (chunk_idx, &chunk_offset) in chunk_offsets.iter().enumerate() {
        let chunk_number = (chunk_idx + 1) as u32;
        while stsc_index + 1 < sample_to_chunk.len()
            && chunk_number >= sample_to_chunk[stsc_index + 1].first_chunk
        {
            stsc_index += 1;
        }

        let samples_per_chunk = sample_to_chunk[stsc_index].samples_per_chunk as usize;
        if samples_per_chunk == 0 {
            return Vec::new();
        }

        let mut sample_offset = chunk_offset;
        for _ in 0..samples_per_chunk {
            let Some(sample_size) = sample_sizes.size_at(sample_index) else {
                return ranges;
            };
            if sample_size == 0 {
                return Vec::new();
            }
            ranges.push((sample_offset, sample_size));
            sample_offset = match sample_offset.checked_add(sample_size as u64) {
                Some(v) => v,
                None => return Vec::new(),
            };
            sample_index += 1;
            if sample_index >= sample_sizes.len() {
                return ranges;
            }
        }
    }

    ranges
}

pub(super) fn parse_stsz(
    source: &impl BlockingBinaryReader,
    header: FileBoxHeader,
) -> Option<SampleSizeTable> {
    let bytes = source.read_range(
        header.data_start(),
        (header.size - header.header_len) as usize,
    )?;
    if bytes.len() < 12 {
        return None;
    }

    let constant_sample_size = read_u32_be(&bytes[4..8])?;
    let sample_count = read_u32_be(&bytes[8..12])?;
    if sample_count == 0 {
        return None;
    }
    if constant_sample_size != 0 {
        return Some(SampleSizeTable {
            constant_size: Some(constant_sample_size as usize),
            sizes: Vec::new(),
            sample_count: sample_count as usize,
        });
    }
    let sample_count = sample_count as usize;
    let required_len = 12usize.checked_add(sample_count.checked_mul(4)?)?;
    if bytes.len() < required_len {
        return None;
    }

    let mut sizes = Vec::with_capacity(sample_count);
    for chunk in bytes[12..required_len].chunks_exact(4) {
        let size = read_u32_be(chunk)? as usize;
        if size == 0 {
            return None;
        }
        sizes.push(size);
    }

    Some(SampleSizeTable {
        constant_size: None,
        sizes,
        sample_count,
    })
}

pub(super) fn parse_stz2(
    source: &impl BlockingBinaryReader,
    header: FileBoxHeader,
) -> Option<SampleSizeTable> {
    let bytes = source.read_range(
        header.data_start(),
        (header.size - header.header_len) as usize,
    )?;
    parse_stz2_bytes(&bytes)
}

pub(super) fn parse_stz2_bytes(bytes: &[u8]) -> Option<SampleSizeTable> {
    if bytes.len() < 12 {
        return None;
    }

    let field_size = bytes[7];
    let sample_count = read_u32_be(&bytes[8..12])? as usize;
    if sample_count == 0 {
        return None;
    }

    let mut sizes = Vec::with_capacity(sample_count);
    match field_size {
        4 => {
            let required_len = 12usize.checked_add(sample_count.div_ceil(2))?;
            if bytes.len() < required_len {
                return None;
            }
            for byte in &bytes[12..required_len] {
                let hi = (byte >> 4) as usize;
                let lo = (byte & 0x0F) as usize;
                if sizes.len() < sample_count {
                    if hi == 0 {
                        return None;
                    }
                    sizes.push(hi);
                }
                if sizes.len() < sample_count {
                    if lo == 0 {
                        return None;
                    }
                    sizes.push(lo);
                }
            }
        }
        8 => {
            let required_len = 12usize.checked_add(sample_count)?;
            if bytes.len() < required_len {
                return None;
            }
            for size in &bytes[12..required_len] {
                if *size == 0 {
                    return None;
                }
                sizes.push(*size as usize);
            }
        }
        16 => {
            let required_len = 12usize.checked_add(sample_count.checked_mul(2)?)?;
            if bytes.len() < required_len {
                return None;
            }
            for chunk in bytes[12..required_len].chunks_exact(2) {
                let size = read_u16_endian(chunk, Endian::Big)? as usize;
                if size == 0 {
                    return None;
                }
                sizes.push(size);
            }
        }
        _ => return None,
    }

    Some(SampleSizeTable {
        constant_size: None,
        sizes,
        sample_count,
    })
}

/// Chunk offsets from an `stco` (32-bit, `entry_bytes` 4) or `co64` (64-bit, 8) box.
pub(super) fn parse_chunk_offsets(
    source: &impl BlockingBinaryReader,
    header: FileBoxHeader,
    entry_bytes: usize,
) -> Option<Vec<u64>> {
    let bytes = source.read_range(
        header.data_start(),
        (header.size - header.header_len) as usize,
    )?;
    if bytes.len() < 8 {
        return None;
    }
    let entry_count = read_u32_be(&bytes[4..8])? as usize;
    if entry_count == 0 {
        return None;
    }
    let required_len = 8usize.checked_add(entry_count.checked_mul(entry_bytes)?)?;
    bytes
        .get(8..required_len)?
        .chunks_exact(entry_bytes)
        .map(|chunk| {
            if entry_bytes == 8 {
                read_u64_be(chunk)
            } else {
                read_u32_be(chunk).map(u64::from)
            }
        })
        .collect()
}

pub(super) fn parse_stsc(
    source: &impl BlockingBinaryReader,
    header: FileBoxHeader,
) -> Option<Vec<SampleToChunkEntry>> {
    let bytes = source.read_range(
        header.data_start(),
        (header.size - header.header_len) as usize,
    )?;
    if bytes.len() < 8 {
        return None;
    }
    let entry_count = read_u32_be(&bytes[4..8])? as usize;
    if entry_count == 0 {
        return None;
    }
    let required_len = 8usize.checked_add(entry_count.checked_mul(12)?)?;
    if bytes.len() < required_len {
        return None;
    }

    let mut entries = Vec::with_capacity(entry_count);
    let mut last_first_chunk = 0u32;
    for chunk in bytes[8..required_len].chunks_exact(12) {
        let first_chunk = read_u32_be(&chunk[0..4])?;
        let samples_per_chunk = read_u32_be(&chunk[4..8])?;
        if first_chunk == 0 || samples_per_chunk == 0 || first_chunk <= last_first_chunk {
            return None;
        }
        last_first_chunk = first_chunk;
        entries.push(SampleToChunkEntry {
            first_chunk,
            samples_per_chunk,
        });
    }
    Some(entries)
}
