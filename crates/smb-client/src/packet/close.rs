use crate::packet::create::{Smb2FileAttributes, Smb2FileId};
use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{le_u16, le_u32, le_u64};
use crate::SmbClientError;

const CLOSE_REQUEST_STRUCTURE_SIZE: u16 = 24;
const CLOSE_RESPONSE_STRUCTURE_SIZE: u16 = 60;
const CLOSE_REQUEST_FIXED_BYTES: usize = 24;
const CLOSE_RESPONSE_FIXED_BYTES: usize = 60;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2CloseFlags(pub u16);

impl Smb2CloseFlags {
    pub const POSTQUERY_ATTRIB: Self = Self(0x0001);

    pub fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2CloseRequest {
    pub flags: Smb2CloseFlags,
    pub file_id: Smb2FileId,
}

impl Smb2CloseRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(CLOSE_REQUEST_FIXED_BYTES);
        push_u16(&mut bytes, CLOSE_REQUEST_STRUCTURE_SIZE);
        push_u16(&mut bytes, self.flags.0);
        push_u32(&mut bytes, 0);
        push_u64(&mut bytes, self.file_id.persistent);
        push_u64(&mut bytes, self.file_id.volatile);
        bytes
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        if body.len() < CLOSE_REQUEST_FIXED_BYTES {
            return Err(SmbClientError::ShortFrame {
                expected: CLOSE_REQUEST_FIXED_BYTES,
                actual: body.len(),
            });
        }
        let structure_size = le_u16(body, 0);
        if structure_size != CLOSE_REQUEST_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        Ok(Self {
            flags: Smb2CloseFlags(le_u16(body, 2)),
            file_id: Smb2FileId {
                persistent: le_u64(body, 8),
                volatile: le_u64(body, 16),
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2CloseResponse {
    pub flags: Smb2CloseFlags,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub change_time: u64,
    pub allocation_size: u64,
    pub end_of_file: u64,
    pub file_attributes: Smb2FileAttributes,
}

impl Smb2CloseResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body_offset = SMB2_HEADER_BYTES;
        let required_len = body_offset + CLOSE_RESPONSE_FIXED_BYTES;
        if packet.len() < required_len {
            return Err(SmbClientError::ShortFrame {
                expected: required_len,
                actual: packet.len(),
            });
        }
        let body = &packet[body_offset..];
        let structure_size = le_u16(body, 0);
        if structure_size != CLOSE_RESPONSE_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        Ok(Self {
            flags: Smb2CloseFlags(le_u16(body, 2)),
            creation_time: le_u64(body, 8),
            last_access_time: le_u64(body, 16),
            last_write_time: le_u64(body, 24),
            change_time: le_u64(body, 32),
            allocation_size: le_u64(body, 40),
            end_of_file: le_u64(body, 48),
            file_attributes: Smb2FileAttributes(le_u32(body, 56)),
        })
    }
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn close_request_round_trips_file_id() {
        let request = Smb2CloseRequest {
            flags: Smb2CloseFlags::POSTQUERY_ATTRIB,
            file_id: Smb2FileId {
                persistent: 0x0102_0304_0506_0708,
                volatile: 0x1112_1314_1516_1718,
            },
        };

        let encoded = request.encode();
        let decoded = Smb2CloseRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), CLOSE_REQUEST_STRUCTURE_SIZE);
        assert_eq!(le_u64(&encoded, 8), request.file_id.persistent);
        assert_eq!(le_u64(&encoded, 16), request.file_id.volatile);
    }

    #[test]
    fn close_request_rejects_short_body() {
        assert_eq!(
            Smb2CloseRequest::decode(&[0; CLOSE_REQUEST_FIXED_BYTES - 1]),
            Err(SmbClientError::ShortFrame {
                expected: CLOSE_REQUEST_FIXED_BYTES,
                actual: CLOSE_REQUEST_FIXED_BYTES - 1
            })
        );
    }

    #[test]
    fn close_response_decodes_postquery_attributes() {
        let mut packet = Smb2Header::new(Smb2Command::Close, 21).encode().to_vec();
        packet.extend_from_slice(&response_body());

        let response = Smb2CloseResponse::decode_packet(&packet).expect("decode");

        assert!(response.flags.contains(Smb2CloseFlags::POSTQUERY_ATTRIB));
        assert_eq!(response.creation_time, 1);
        assert_eq!(response.last_access_time, 2);
        assert_eq!(response.last_write_time, 3);
        assert_eq!(response.change_time, 4);
        assert_eq!(response.allocation_size, 4096);
        assert_eq!(response.end_of_file, 128);
        assert_eq!(response.file_attributes, Smb2FileAttributes::ARCHIVE);
    }

    #[test]
    fn close_response_rejects_bad_structure_size() {
        let mut packet = Smb2Header::new(Smb2Command::Close, 21).encode().to_vec();
        packet.extend_from_slice(&response_body());
        packet[SMB2_HEADER_BYTES..SMB2_HEADER_BYTES + 2].copy_from_slice(&59u16.to_le_bytes());

        assert_eq!(
            Smb2CloseResponse::decode_packet(&packet),
            Err(SmbClientError::InvalidStructureSize { size: 59 })
        );
    }

    fn response_body() -> Vec<u8> {
        let mut body = Vec::with_capacity(CLOSE_RESPONSE_FIXED_BYTES);
        push_u16(&mut body, CLOSE_RESPONSE_STRUCTURE_SIZE);
        push_u16(&mut body, Smb2CloseFlags::POSTQUERY_ATTRIB.0);
        push_u32(&mut body, 0);
        push_u64(&mut body, 1);
        push_u64(&mut body, 2);
        push_u64(&mut body, 3);
        push_u64(&mut body, 4);
        push_u64(&mut body, 4096);
        push_u64(&mut body, 128);
        push_u32(&mut body, Smb2FileAttributes::ARCHIVE.0);
        body
    }
}
