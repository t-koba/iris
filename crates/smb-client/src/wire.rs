//! Little-endian field readers and bounds-checked slicing shared by the SMB2 decoders.

use crate::packet::header::SMB2_HEADER_BYTES;
use crate::SmbClientError;

fn le_bytes<const N: usize>(bytes: &[u8], offset: usize) -> [u8; N] {
    bytes[offset..offset + N]
        .try_into()
        .expect("slice length equals N")
}

pub(crate) fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(le_bytes(bytes, offset))
}

pub(crate) fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(le_bytes(bytes, offset))
}

pub(crate) fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(le_bytes(bytes, offset))
}

pub(crate) fn checked_slice(
    bytes: &[u8],
    offset: usize,
    length: usize,
) -> Result<&[u8], SmbClientError> {
    offset
        .checked_add(length)
        .and_then(|end| bytes.get(offset..end))
        .ok_or(SmbClientError::BufferRangeOutOfBounds {
            offset,
            length,
            available: bytes.len(),
        })
}

/// `absolute_offset` counts from the start of the SMB2 header and must not point into the fixed part.
pub(crate) fn checked_body_tail_slice(
    body: &[u8],
    absolute_offset: usize,
    length: usize,
    fixed_bytes: usize,
) -> Result<&[u8], SmbClientError> {
    if absolute_offset < SMB2_HEADER_BYTES + fixed_bytes {
        return Err(SmbClientError::BufferRangeOutOfBounds {
            offset: absolute_offset,
            length,
            available: SMB2_HEADER_BYTES + body.len(),
        });
    }
    checked_slice(body, absolute_offset - SMB2_HEADER_BYTES, length)
}

pub(crate) fn decode_utf16le(bytes: &[u8]) -> Result<String, SmbClientError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(SmbClientError::InvalidUtf16Length {
            length: bytes.len(),
        });
    }
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| SmbClientError::InvalidUtf16Path)
}

pub(crate) fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
pub(crate) fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
pub(crate) fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Rounds `value` up to the next multiple of 8, the alignment of SMB2 variable fields.
pub(crate) fn align8(value: usize) -> usize {
    value.saturating_add(7) & !7
}

pub(crate) fn encode_utf16le(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

pub(crate) fn checked_u16_len(field: &'static str, length: usize) -> Result<u16, SmbClientError> {
    u16::try_from(length).map_err(|_| SmbClientError::FieldTooLarge {
        field,
        length,
        max: u16::MAX as usize,
    })
}

pub(crate) fn checked_u32_len(field: &'static str, length: usize) -> Result<u32, SmbClientError> {
    u32::try_from(length).map_err(|_| SmbClientError::FieldTooLarge {
        field,
        length,
        max: u32::MAX as usize,
    })
}

/// Checks the fixed length and StructureSize of an SMB2 message body.
pub(crate) fn check_fixed_body(
    body: &[u8],
    fixed_bytes: usize,
    structure_size: u16,
) -> Result<(), SmbClientError> {
    if body.len() < fixed_bytes {
        return Err(SmbClientError::ShortFrame {
            expected: fixed_bytes,
            actual: body.len(),
        });
    }
    let size = le_u16(body, 0);
    if size != structure_size {
        return Err(SmbClientError::InvalidStructureSize { size });
    }
    Ok(())
}

/// The body of an SMB2 response after checking its fixed length and StructureSize.
pub(crate) fn response_body(
    packet: &[u8],
    fixed_bytes: usize,
    structure_size: u16,
) -> Result<&[u8], SmbClientError> {
    let required_len = SMB2_HEADER_BYTES + fixed_bytes;
    if packet.len() < required_len {
        return Err(SmbClientError::ShortFrame {
            expected: required_len,
            actual: packet.len(),
        });
    }
    let body = &packet[SMB2_HEADER_BYTES..];
    check_fixed_body(body, fixed_bytes, structure_size)?;
    Ok(body)
}
