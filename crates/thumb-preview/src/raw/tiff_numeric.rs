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

pub(super) fn parse_tiff_string_value(mut bytes: Vec<u8>) -> Option<String> {
    while bytes.last().copied() == Some(0) {
        bytes.pop();
    }
    let value = String::from_utf8_lossy(&bytes).trim().to_string();
    (!value.is_empty()).then_some(value)
}
