use super::*;

pub(super) fn contiguous_segment(offsets: &[u32], counts: &[u32]) -> Option<(u64, usize)> {
    if offsets.is_empty() || offsets.len() != counts.len() {
        return None;
    }

    let start = offsets[0] as u64;
    let mut expected = start;
    let mut total = 0usize;
    for (&offset, &count) in offsets.iter().zip(counts.iter()) {
        if count == 0 || offset as u64 != expected {
            return None;
        }
        total = total.checked_add(count as usize)?;
        expected = expected.checked_add(count as u64)?;
    }

    Some((start, total))
}

pub(super) fn validate_segment_layout(
    offsets: &[u32],
    counts: &[u32],
    source_len: u64,
) -> Option<usize> {
    if offsets.is_empty() || offsets.len() != counts.len() {
        return None;
    }

    let mut total = 0usize;
    for (&offset, &count) in offsets.iter().zip(counts.iter()) {
        if count == 0 {
            return None;
        }
        let end = (offset as u64).checked_add(count as u64)?;
        if end > source_len {
            return None;
        }
        total = total.checked_add(count as usize)?;
    }

    Some(total)
}

pub(super) fn contiguous_segment_u64(offsets: &[u64], counts: &[u64]) -> Option<(u64, usize)> {
    if offsets.is_empty() || offsets.len() != counts.len() {
        return None;
    }

    let start = offsets[0];
    let mut expected = start;
    let mut total = 0usize;
    for (&offset, &count) in offsets.iter().zip(counts.iter()) {
        if count == 0 || offset != expected {
            return None;
        }
        total = total.checked_add(usize::try_from(count).ok()?)?;
        expected = expected.checked_add(count)?;
    }

    Some((start, total))
}

pub(super) fn validate_segment_layout_u64(
    offsets: &[u64],
    counts: &[u64],
    source_len: u64,
) -> Option<usize> {
    if offsets.is_empty() || offsets.len() != counts.len() {
        return None;
    }

    let mut total = 0usize;
    for (&offset, &count) in offsets.iter().zip(counts.iter()) {
        if count == 0 {
            return None;
        }
        let end = offset.checked_add(count)?;
        if end > source_len {
            return None;
        }
        total = total.checked_add(usize::try_from(count).ok()?)?;
    }

    Some(total)
}

pub(super) fn inline_tiff_value_bytes(
    endian: Endian,
    value_or_offset: u32,
    byte_len: usize,
) -> Vec<u8> {
    let raw = match endian {
        Endian::Little => value_or_offset.to_le_bytes(),
        Endian::Big => value_or_offset.to_be_bytes(),
    };
    raw[..byte_len].to_vec()
}

pub(super) fn inline_tiff_value_bytes_u64(
    endian: Endian,
    value_or_offset: u64,
    byte_len: usize,
) -> Vec<u8> {
    let raw = match endian {
        Endian::Little => value_or_offset.to_le_bytes(),
        Endian::Big => value_or_offset.to_be_bytes(),
    };
    raw[..byte_len].to_vec()
}

pub(super) fn tiff_type_size(field_type: u16) -> Option<usize> {
    Some(match field_type {
        TIFF_TYPE_BYTE | TIFF_TYPE_ASCII | TIFF_TYPE_SBYTE | TIFF_TYPE_UNDEFINED => 1,
        TIFF_TYPE_SHORT | TIFF_TYPE_SSHORT => 2,
        TIFF_TYPE_LONG | TIFF_TYPE_SLONG | TIFF_TYPE_FLOAT | TIFF_TYPE_IFD => 4,
        TIFF_TYPE_RATIONAL | TIFF_TYPE_SRATIONAL | TIFF_TYPE_DOUBLE | TIFF_TYPE_LONG8
        | TIFF_TYPE_SLONG8 | TIFF_TYPE_IFD8 => 8,
        _ => return None,
    })
}
