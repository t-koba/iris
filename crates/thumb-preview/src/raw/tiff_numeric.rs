use super::*;

pub(super) fn parse_tiff_endian(header: &[u8]) -> Option<Endian> {
    if header.len() < 2 {
        return None;
    }
    match (header[0], header[1]) {
        (b'I', b'I') => Some(Endian::Little),
        (b'M', b'M') => Some(Endian::Big),
        _ => None,
    }
}

pub(super) fn read_u16_endian(bytes: &[u8], endian: Endian) -> Option<u16> {
    if bytes.len() < 2 {
        return None;
    }
    Some(match endian {
        Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
        Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
    })
}

pub(super) fn read_u32_endian(bytes: &[u8], endian: Endian) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(match endian {
        Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    })
}

pub(super) fn read_u64_endian(bytes: &[u8], endian: Endian) -> Option<u64> {
    if bytes.len() < 8 {
        return None;
    }
    Some(match endian {
        Endian::Little => u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        Endian::Big => u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
    })
}

#[cfg(feature = "raw-full")]
pub(super) fn read_u32_le(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(super) fn read_u32_be(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(super) fn read_u64_be(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 8 {
        return None;
    }
    Some(u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

pub(super) fn read_first_u32_tag_value(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    field_type: u16,
    count: u32,
    value_or_offset: u32,
) -> Option<u32> {
    if count == 0 {
        return None;
    }

    match field_type {
        TIFF_TYPE_LONG | TIFF_TYPE_IFD => {
            if count == 1 {
                Some(value_or_offset)
            } else {
                let bytes = source.read_range(value_or_offset as u64, 4)?;
                read_u32_endian(&bytes, endian)
            }
        }
        TIFF_TYPE_SHORT => {
            if count == 1 {
                Some(match endian {
                    Endian::Little => value_or_offset & 0xFFFF,
                    Endian::Big => value_or_offset >> 16,
                })
            } else if (count as usize) * 2 <= 4 {
                let bytes = inline_tiff_value_bytes(endian, value_or_offset, (count as usize) * 2);
                read_u16_endian(&bytes[..2], endian).map(u32::from)
            } else {
                let bytes = source.read_range(value_or_offset as u64, 2)?;
                read_u16_endian(&bytes, endian).map(u32::from)
            }
        }
        _ => None,
    }
}

pub(super) fn read_first_u64_tag_value_bigtiff(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    field_type: u16,
    count: u64,
    value_or_offset: u64,
) -> Option<u64> {
    if count == 0 {
        return None;
    }

    let unit_size = tiff_type_size(field_type)?;
    let total_len = usize::try_from(count).ok()?.checked_mul(unit_size)?;
    let inline = if total_len <= BIGTIFF_INLINE_BYTES {
        Some(inline_tiff_value_bytes_u64(
            endian,
            value_or_offset,
            total_len,
        ))
    } else {
        None
    };

    match field_type {
        TIFF_TYPE_LONG | TIFF_TYPE_IFD => {
            if let Some(bytes) = inline {
                read_u32_endian(&bytes[..4], endian).map(u64::from)
            } else {
                let bytes = source.read_range(value_or_offset, 4)?;
                read_u32_endian(&bytes, endian).map(u64::from)
            }
        }
        TIFF_TYPE_LONG8 | TIFF_TYPE_IFD8 => {
            if let Some(bytes) = inline {
                read_u64_endian(&bytes[..8], endian)
            } else {
                let bytes = source.read_range(value_or_offset, 8)?;
                read_u64_endian(&bytes, endian)
            }
        }
        TIFF_TYPE_SHORT => {
            if let Some(bytes) = inline {
                read_u16_endian(&bytes[..2], endian).map(u64::from)
            } else {
                let bytes = source.read_range(value_or_offset, 2)?;
                read_u16_endian(&bytes, endian).map(u64::from)
            }
        }
        _ => None,
    }
}

pub(super) fn read_u32_tag_array(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    field_type: u16,
    count: u32,
    value_or_offset: u32,
) -> Vec<u32> {
    if count == 0 || count as usize > MAX_TAG_ARRAY_COUNT {
        return Vec::new();
    }

    match field_type {
        TIFF_TYPE_LONG | TIFF_TYPE_IFD => {
            if count == 1 {
                return vec![value_or_offset];
            }

            let byte_len = count as usize * 4;
            let Some(bytes) = source.read_range(value_or_offset as u64, byte_len) else {
                return Vec::new();
            };

            let mut values = Vec::with_capacity(count as usize);
            for chunk in bytes.chunks_exact(4) {
                if let Some(v) = read_u32_endian(chunk, endian) {
                    values.push(v);
                }
            }
            values
        }
        TIFF_TYPE_SHORT => {
            if count == 1 {
                let value = match endian {
                    Endian::Little => value_or_offset & 0xFFFF,
                    Endian::Big => value_or_offset >> 16,
                };
                return vec![value];
            }

            let byte_len = count as usize * 2;
            let bytes = if byte_len <= 4 {
                inline_tiff_value_bytes(endian, value_or_offset, byte_len)
            } else {
                let Some(bytes) = source.read_range(value_or_offset as u64, byte_len) else {
                    return Vec::new();
                };
                bytes
            };

            let mut values = Vec::with_capacity(count as usize);
            for chunk in bytes.chunks_exact(2) {
                if let Some(v) = read_u16_endian(chunk, endian) {
                    values.push(v as u32);
                }
            }
            values
        }
        _ => Vec::new(),
    }
}

pub(super) fn read_u64_tag_array_bigtiff(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    field_type: u16,
    count: u64,
    value_or_offset: u64,
) -> Vec<u64> {
    if count == 0 || count as usize > MAX_TAG_ARRAY_COUNT {
        return Vec::new();
    }

    let unit_size = match field_type {
        TIFF_TYPE_LONG | TIFF_TYPE_IFD => 4,
        TIFF_TYPE_LONG8 | TIFF_TYPE_IFD8 => 8,
        TIFF_TYPE_SHORT => 2,
        _ => return Vec::new(),
    };
    let byte_len = match usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(unit_size))
    {
        Some(v) => v,
        None => return Vec::new(),
    };

    let bytes = if byte_len <= BIGTIFF_INLINE_BYTES {
        inline_tiff_value_bytes_u64(endian, value_or_offset, byte_len)
    } else {
        let Some(bytes) = source.read_range(value_or_offset, byte_len) else {
            return Vec::new();
        };
        bytes
    };

    let mut values = Vec::with_capacity(count as usize);
    match field_type {
        TIFF_TYPE_LONG | TIFF_TYPE_IFD => {
            for chunk in bytes.chunks_exact(4) {
                if let Some(v) = read_u32_endian(chunk, endian) {
                    values.push(v as u64);
                }
            }
        }
        TIFF_TYPE_LONG8 | TIFF_TYPE_IFD8 => {
            for chunk in bytes.chunks_exact(8) {
                if let Some(v) = read_u64_endian(chunk, endian) {
                    values.push(v);
                }
            }
        }
        TIFF_TYPE_SHORT => {
            for chunk in bytes.chunks_exact(2) {
                if let Some(v) = read_u16_endian(chunk, endian) {
                    values.push(v as u64);
                }
            }
        }
        _ => {}
    }
    values
}

pub(super) fn read_tiff_ascii_tag_value(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    field_type: u16,
    count: u32,
    value_or_offset: u32,
) -> Option<String> {
    let count = usize::try_from(count).ok()?;
    let bytes = read_tiff_field_bytes_standard(source, endian, field_type, count, value_or_offset)?;
    parse_tiff_string_value(bytes)
}

pub(super) fn read_tiff_ascii_tag_value_bigtiff(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    field_type: u16,
    count: u64,
    value_or_offset: u64,
) -> Option<String> {
    let count = usize::try_from(count).ok()?;
    let bytes = read_tiff_field_bytes_bigtiff(source, endian, field_type, count, value_or_offset)?;
    parse_tiff_string_value(bytes)
}

pub(super) fn read_tiff_field_bytes_standard(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    field_type: u16,
    count: usize,
    value_or_offset: u32,
) -> Option<Vec<u8>> {
    let unit_size = tiff_type_size(field_type)?;
    let total_len = count.checked_mul(unit_size)?;
    if total_len == 0 {
        return None;
    }

    if total_len <= 4 {
        return Some(inline_tiff_value_bytes(endian, value_or_offset, total_len));
    }
    source.read_range(value_or_offset as u64, total_len)
}

pub(super) fn read_tiff_field_bytes_bigtiff(
    source: &impl BlockingBinaryReader,
    endian: Endian,
    field_type: u16,
    count: usize,
    value_or_offset: u64,
) -> Option<Vec<u8>> {
    let unit_size = tiff_type_size(field_type)?;
    let total_len = count.checked_mul(unit_size)?;
    if total_len == 0 {
        return None;
    }

    if total_len <= BIGTIFF_INLINE_BYTES {
        return Some(inline_tiff_value_bytes_u64(
            endian,
            value_or_offset,
            total_len,
        ));
    }
    source.read_range(value_or_offset, total_len)
}

pub(super) fn parse_tiff_string_value(mut bytes: Vec<u8>) -> Option<String> {
    while bytes.last().copied() == Some(0) {
        bytes.pop();
    }
    let value = String::from_utf8_lossy(&bytes).trim().to_string();
    (!value.is_empty()).then_some(value)
}
