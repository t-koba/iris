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
