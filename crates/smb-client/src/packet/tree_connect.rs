use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{checked_slice, decode_utf16le, le_u16, le_u32};
use crate::SmbClientError;

const TREE_CONNECT_REQUEST_STRUCTURE_SIZE: u16 = 9;
const TREE_CONNECT_RESPONSE_STRUCTURE_SIZE: u16 = 16;
const TREE_CONNECT_REQUEST_FIXED_BYTES: usize = 8;
const TREE_CONNECT_RESPONSE_FIXED_BYTES: usize = 16;
const U16_MAX_USIZE: usize = u16::MAX as usize;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2TreeConnectFlags(pub u16);

impl Smb2TreeConnectFlags {
    pub const REDIRECT_TO_OWNER: Self = Self(0x0002);
    pub const EXTENSION_PRESENT: Self = Self(0x0004);

    pub fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Smb2ShareType {
    Disk = 0x01,
    Pipe = 0x02,
    Print = 0x03,
}

impl Smb2ShareType {
    pub fn from_wire(value: u8) -> Result<Self, SmbClientError> {
        match value {
            0x01 => Ok(Self::Disk),
            0x02 => Ok(Self::Pipe),
            0x03 => Ok(Self::Print),
            share_type => Err(SmbClientError::UnknownShareType { share_type }),
        }
    }

    pub fn to_wire(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2ShareFlags(pub u32);

impl Smb2ShareFlags {
    pub const DFS: Self = Self(0x0000_0001);
    pub const ENCRYPT_DATA: Self = Self(0x0000_8000);

    pub fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2ShareCapabilities(pub u32);

impl Smb2ShareCapabilities {
    pub const DFS: Self = Self(0x0000_0008);
    pub const CONTINUOUS_AVAILABILITY: Self = Self(0x0000_0010);

    pub fn contains(self, capabilities: Self) -> bool {
        self.0 & capabilities.0 == capabilities.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2TreeConnectRequest {
    pub flags: Smb2TreeConnectFlags,
    pub path: String,
}

impl Smb2TreeConnectRequest {
    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let path_bytes = encode_utf16le(&self.path);
        let path_length = checked_u16_len("tree_connect.path", path_bytes.len())?;
        let path_offset = (SMB2_HEADER_BYTES + TREE_CONNECT_REQUEST_FIXED_BYTES) as u16;
        let mut bytes = Vec::with_capacity(TREE_CONNECT_REQUEST_FIXED_BYTES + path_bytes.len());
        push_u16(&mut bytes, TREE_CONNECT_REQUEST_STRUCTURE_SIZE);
        push_u16(&mut bytes, self.flags.0);
        push_u16(&mut bytes, path_offset);
        push_u16(&mut bytes, path_length);
        bytes.extend_from_slice(&path_bytes);
        Ok(bytes)
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        if body.len() < TREE_CONNECT_REQUEST_FIXED_BYTES {
            return Err(SmbClientError::ShortFrame {
                expected: TREE_CONNECT_REQUEST_FIXED_BYTES,
                actual: body.len(),
            });
        }
        let structure_size = le_u16(body, 0);
        if structure_size != TREE_CONNECT_REQUEST_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        let path_offset = le_u16(body, 4) as usize;
        let path_length = le_u16(body, 6) as usize;
        let body_path_offset = path_offset.checked_sub(SMB2_HEADER_BYTES).ok_or(
            SmbClientError::BufferRangeOutOfBounds {
                offset: path_offset,
                length: path_length,
                available: SMB2_HEADER_BYTES + body.len(),
            },
        )?;
        let path_bytes = checked_slice(body, body_path_offset, path_length)?;
        let path = decode_utf16le(path_bytes)?;

        Ok(Self {
            flags: Smb2TreeConnectFlags(le_u16(body, 2)),
            path,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2TreeConnectResponse {
    pub share_type: Smb2ShareType,
    pub share_flags: Smb2ShareFlags,
    pub capabilities: Smb2ShareCapabilities,
    pub maximal_access: u32,
}

impl Smb2TreeConnectResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body_offset = SMB2_HEADER_BYTES;
        let required_len = body_offset + TREE_CONNECT_RESPONSE_FIXED_BYTES;
        if packet.len() < required_len {
            return Err(SmbClientError::ShortFrame {
                expected: required_len,
                actual: packet.len(),
            });
        }
        let body = &packet[body_offset..];
        let structure_size = le_u16(body, 0);
        if structure_size != TREE_CONNECT_RESPONSE_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        Ok(Self {
            share_type: Smb2ShareType::from_wire(body[2])?,
            share_flags: Smb2ShareFlags(le_u32(body, 4)),
            capabilities: Smb2ShareCapabilities(le_u32(body, 8)),
            maximal_access: le_u32(body, 12),
        })
    }
}

fn checked_u16_len(field: &'static str, length: usize) -> Result<u16, SmbClientError> {
    u16::try_from(length).map_err(|_| SmbClientError::FieldTooLarge {
        field,
        length,
        max: U16_MAX_USIZE,
    })
}

fn encode_utf16le(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len() * 2);
    for unit in value.encode_utf16() {
        push_u16(&mut bytes, unit);
    }
    bytes
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn tree_connect_request_round_trips_arbitrary_unicode(
            path in prop::collection::vec(any::<char>(), 0..512)
                .prop_map(|characters| characters.into_iter().collect::<String>())
        ) {
            let request = Smb2TreeConnectRequest {
                flags: Smb2TreeConnectFlags::EXTENSION_PRESENT,
                path,
            };

            let encoded = request.encode().expect("bounded path should encode");
            let decoded = Smb2TreeConnectRequest::decode(&encoded)
                .expect("encoded path should decode");

            prop_assert_eq!(decoded, request);
        }
    }

    #[test]
    fn tree_connect_request_round_trips_unc_path() {
        let request = Smb2TreeConnectRequest {
            flags: Smb2TreeConnectFlags::REDIRECT_TO_OWNER,
            path: "\\\\server\\共有".to_string(),
        };

        let encoded = request.encode().expect("encode");
        let decoded = Smb2TreeConnectRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), TREE_CONNECT_REQUEST_STRUCTURE_SIZE);
        assert_eq!(
            le_u16(&encoded, 4),
            (SMB2_HEADER_BYTES + TREE_CONNECT_REQUEST_FIXED_BYTES) as u16
        );
        assert_eq!(le_u16(&encoded, 6), 22);
    }

    #[test]
    fn tree_connect_request_rejects_path_before_body() {
        let request = Smb2TreeConnectRequest {
            flags: Smb2TreeConnectFlags::default(),
            path: "\\\\server\\share".to_string(),
        };
        let mut encoded = request.encode().expect("encode");
        encoded[4..6].copy_from_slice(&(SMB2_HEADER_BYTES as u16 - 1).to_le_bytes());

        assert_eq!(
            Smb2TreeConnectRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES - 1,
                length: 28,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn tree_connect_request_rejects_odd_utf16_path_length() {
        let request = Smb2TreeConnectRequest {
            flags: Smb2TreeConnectFlags::default(),
            path: "\\\\server\\share".to_string(),
        };
        let mut encoded = request.encode().expect("encode");
        encoded[6..8].copy_from_slice(&27u16.to_le_bytes());

        assert_eq!(
            Smb2TreeConnectRequest::decode(&encoded),
            Err(SmbClientError::InvalidUtf16Length { length: 27 })
        );
    }

    #[test]
    fn tree_connect_request_rejects_invalid_utf16_path() {
        let mut encoded = Vec::new();
        push_u16(&mut encoded, TREE_CONNECT_REQUEST_STRUCTURE_SIZE);
        push_u16(&mut encoded, Smb2TreeConnectFlags::default().0);
        push_u16(
            &mut encoded,
            (SMB2_HEADER_BYTES + TREE_CONNECT_REQUEST_FIXED_BYTES) as u16,
        );
        push_u16(&mut encoded, 2);
        push_u16(&mut encoded, 0xd800);

        assert_eq!(
            Smb2TreeConnectRequest::decode(&encoded),
            Err(SmbClientError::InvalidUtf16Path)
        );
    }

    #[test]
    fn tree_connect_request_rejects_too_large_path() {
        let request = Smb2TreeConnectRequest {
            flags: Smb2TreeConnectFlags::default(),
            path: "x".repeat(32768),
        };

        assert_eq!(
            request.encode(),
            Err(SmbClientError::FieldTooLarge {
                field: "tree_connect.path",
                length: 65536,
                max: u16::MAX as usize
            })
        );
    }

    #[test]
    fn tree_connect_response_decodes_share_properties() {
        let mut packet = Smb2Header::new(Smb2Command::TreeConnect, 8)
            .encode()
            .to_vec();
        packet.extend_from_slice(&response_body(
            Smb2ShareType::Disk,
            Smb2ShareFlags::DFS.0 | Smb2ShareFlags::ENCRYPT_DATA.0,
            Smb2ShareCapabilities::DFS.0 | Smb2ShareCapabilities::CONTINUOUS_AVAILABILITY.0,
            0x001f_01ff,
        ));

        let response = Smb2TreeConnectResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response.share_type, Smb2ShareType::Disk);
        assert!(response.share_flags.contains(Smb2ShareFlags::DFS));
        assert!(response.share_flags.contains(Smb2ShareFlags::ENCRYPT_DATA));
        assert!(response.capabilities.contains(Smb2ShareCapabilities::DFS));
        assert!(response
            .capabilities
            .contains(Smb2ShareCapabilities::CONTINUOUS_AVAILABILITY));
        assert_eq!(response.maximal_access, 0x001f_01ff);
    }

    #[test]
    fn tree_connect_response_rejects_unknown_share_type() {
        let mut packet = Smb2Header::new(Smb2Command::TreeConnect, 8)
            .encode()
            .to_vec();
        packet.extend_from_slice(&response_body(Smb2ShareType::Disk, 0, 0, 0));
        packet[SMB2_HEADER_BYTES + 2] = 0xff;

        assert_eq!(
            Smb2TreeConnectResponse::decode_packet(&packet),
            Err(SmbClientError::UnknownShareType { share_type: 0xff })
        );
    }

    fn response_body(
        share_type: Smb2ShareType,
        share_flags: u32,
        capabilities: u32,
        maximal_access: u32,
    ) -> Vec<u8> {
        let mut body = Vec::with_capacity(TREE_CONNECT_RESPONSE_FIXED_BYTES);
        push_u16(&mut body, TREE_CONNECT_RESPONSE_STRUCTURE_SIZE);
        body.push(share_type.to_wire());
        body.push(0);
        push_u32(&mut body, share_flags);
        push_u32(&mut body, capabilities);
        push_u32(&mut body, maximal_access);
        body
    }

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}
