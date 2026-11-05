use crate::packet::create::Smb2FileId;
use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{
    check_fixed_body, checked_body_tail_slice, checked_slice, checked_u16_len, decode_utf16le,
    encode_utf16le, le_u16, le_u32, le_u64, push_u16, push_u32, push_u64, response_body,
};
use crate::SmbClientError;

const QUERY_DIRECTORY_REQUEST_STRUCTURE_SIZE: u16 = 33;
const QUERY_DIRECTORY_RESPONSE_STRUCTURE_SIZE: u16 = 9;
const QUERY_DIRECTORY_REQUEST_FIXED_BYTES: usize = 32;
const QUERY_DIRECTORY_RESPONSE_FIXED_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2FileInformationClass(pub u8);

impl Smb2FileInformationClass {
    pub const FILE_DIRECTORY_INFORMATION: Self = Self(0x01);
    pub const FILE_ID_BOTH_DIRECTORY_INFORMATION: Self = Self(0x25);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2QueryDirectoryFlags(pub u8);

impl Smb2QueryDirectoryFlags {
    pub const RESTART_SCANS: Self = Self(0x01);

    pub fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2QueryDirectoryRequest {
    pub file_information_class: Smb2FileInformationClass,
    pub flags: Smb2QueryDirectoryFlags,
    pub file_index: u32,
    pub file_id: Smb2FileId,
    pub file_name: String,
    pub output_buffer_length: u32,
}

impl Smb2QueryDirectoryRequest {
    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let file_name_bytes = encode_utf16le(&self.file_name);
        let file_name_length = checked_u16_len("query_directory.file_name", file_name_bytes.len())?;
        let file_name_offset = if file_name_bytes.is_empty() {
            0
        } else {
            (SMB2_HEADER_BYTES + QUERY_DIRECTORY_REQUEST_FIXED_BYTES) as u16
        };

        let mut bytes =
            Vec::with_capacity(QUERY_DIRECTORY_REQUEST_FIXED_BYTES + file_name_bytes.len());
        push_u16(&mut bytes, QUERY_DIRECTORY_REQUEST_STRUCTURE_SIZE);
        bytes.push(self.file_information_class.0);
        bytes.push(self.flags.0);
        push_u32(&mut bytes, self.file_index);
        push_u64(&mut bytes, self.file_id.persistent);
        push_u64(&mut bytes, self.file_id.volatile);
        push_u16(&mut bytes, file_name_offset);
        push_u16(&mut bytes, file_name_length);
        push_u32(&mut bytes, self.output_buffer_length);
        bytes.extend_from_slice(&file_name_bytes);
        Ok(bytes)
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        check_fixed_body(
            body,
            QUERY_DIRECTORY_REQUEST_FIXED_BYTES,
            QUERY_DIRECTORY_REQUEST_STRUCTURE_SIZE,
        )?;

        let file_name_offset = le_u16(body, 24) as usize;
        let file_name_length = le_u16(body, 26) as usize;
        let file_name = if file_name_length == 0 {
            String::new()
        } else {
            decode_utf16le(checked_body_tail_slice(
                body,
                file_name_offset,
                file_name_length,
                QUERY_DIRECTORY_REQUEST_FIXED_BYTES,
            )?)?
        };

        Ok(Self {
            file_information_class: Smb2FileInformationClass(body[2]),
            flags: Smb2QueryDirectoryFlags(body[3]),
            file_index: le_u32(body, 4),
            file_id: Smb2FileId {
                persistent: le_u64(body, 8),
                volatile: le_u64(body, 16),
            },
            file_name,
            output_buffer_length: le_u32(body, 28),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2QueryDirectoryResponse {
    pub output_buffer: Vec<u8>,
}

impl Smb2QueryDirectoryResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body = response_body(
            packet,
            QUERY_DIRECTORY_RESPONSE_FIXED_BYTES,
            QUERY_DIRECTORY_RESPONSE_STRUCTURE_SIZE,
        )?;

        let output_buffer_offset = le_u16(body, 2) as usize;
        let output_buffer_length = le_u32(body, 4) as usize;
        let output_buffer = if output_buffer_length == 0 {
            Vec::new()
        } else {
            checked_packet_tail_slice(
                packet,
                output_buffer_offset,
                output_buffer_length,
                SMB2_HEADER_BYTES + QUERY_DIRECTORY_RESPONSE_FIXED_BYTES,
            )?
            .to_vec()
        };

        Ok(Self { output_buffer })
    }
}

fn checked_packet_tail_slice(
    packet: &[u8],
    offset: usize,
    length: usize,
    min_offset: usize,
) -> Result<&[u8], SmbClientError> {
    if offset < min_offset {
        return Err(SmbClientError::BufferRangeOutOfBounds {
            offset,
            length,
            available: packet.len(),
        });
    }
    checked_slice(packet, offset, length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn query_directory_request_round_trips_pattern() {
        let request = Smb2QueryDirectoryRequest {
            file_information_class: Smb2FileInformationClass::FILE_ID_BOTH_DIRECTORY_INFORMATION,
            flags: Smb2QueryDirectoryFlags::RESTART_SCANS,
            file_index: 0,
            file_id: Smb2FileId {
                persistent: 0x0102_0304_0506_0708,
                volatile: 0x1112_1314_1516_1718,
            },
            file_name: "*.txt".to_string(),
            output_buffer_length: 1_048_576,
        };

        let encoded = request.encode().expect("encode");
        let decoded = Smb2QueryDirectoryRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), QUERY_DIRECTORY_REQUEST_STRUCTURE_SIZE);
        assert_eq!(
            le_u16(&encoded, 24),
            (SMB2_HEADER_BYTES + QUERY_DIRECTORY_REQUEST_FIXED_BYTES) as u16
        );
        assert_eq!(le_u16(&encoded, 26), 10);
    }

    #[test]
    fn query_directory_request_encodes_empty_pattern() {
        let request = query_request(String::new());

        let encoded = request.encode().expect("encode");
        let decoded = Smb2QueryDirectoryRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 24), 0);
        assert_eq!(le_u16(&encoded, 26), 0);
    }

    #[test]
    fn query_directory_request_rejects_pattern_before_body() {
        let request = query_request("*".to_string());
        let mut encoded = request.encode().expect("encode");
        encoded[24..26].copy_from_slice(&(SMB2_HEADER_BYTES as u16 - 1).to_le_bytes());

        assert_eq!(
            Smb2QueryDirectoryRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES - 1,
                length: 2,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn query_directory_request_rejects_pattern_inside_fixed_body() {
        let request = query_request("*".to_string());
        let mut encoded = request.encode().expect("encode");
        encoded[24..26].copy_from_slice(&(SMB2_HEADER_BYTES as u16).to_le_bytes());

        assert_eq!(
            Smb2QueryDirectoryRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES,
                length: 2,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn query_directory_request_rejects_odd_utf16_pattern_length() {
        let request = query_request("*".to_string());
        let mut encoded = request.encode().expect("encode");
        encoded[26..28].copy_from_slice(&1u16.to_le_bytes());

        assert_eq!(
            Smb2QueryDirectoryRequest::decode(&encoded),
            Err(SmbClientError::InvalidUtf16Length { length: 1 })
        );
    }

    #[test]
    fn query_directory_request_rejects_too_large_pattern() {
        let request = query_request("x".repeat(32768));

        assert_eq!(
            request.encode(),
            Err(SmbClientError::FieldTooLarge {
                field: "query_directory.file_name",
                length: 65536,
                max: u16::MAX as usize
            })
        );
    }

    #[test]
    fn query_directory_response_decodes_buffer() {
        let mut packet = Smb2Header::new(Smb2Command::QueryDirectory, 23)
            .encode()
            .to_vec();
        let output = b"directory-entries";
        packet.extend_from_slice(&response_body(output));
        packet.extend_from_slice(output);

        let response = Smb2QueryDirectoryResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response.output_buffer, output);
    }

    #[test]
    fn query_directory_response_rejects_buffer_outside_packet() {
        let mut packet = Smb2Header::new(Smb2Command::QueryDirectory, 23)
            .encode()
            .to_vec();
        packet.extend_from_slice(&response_body(b"abc"));
        packet[SMB2_HEADER_BYTES + 4..SMB2_HEADER_BYTES + 8].copy_from_slice(&8u32.to_le_bytes());

        assert_eq!(
            Smb2QueryDirectoryResponse::decode_packet(&packet),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + QUERY_DIRECTORY_RESPONSE_FIXED_BYTES,
                length: 8,
                available: packet.len()
            })
        );
    }

    #[test]
    fn query_directory_response_rejects_buffer_inside_fixed_body() {
        let mut packet = Smb2Header::new(Smb2Command::QueryDirectory, 23)
            .encode()
            .to_vec();
        packet.extend_from_slice(&response_body(b"abc"));
        packet[SMB2_HEADER_BYTES + 2..SMB2_HEADER_BYTES + 4]
            .copy_from_slice(&(SMB2_HEADER_BYTES as u16).to_le_bytes());

        assert_eq!(
            Smb2QueryDirectoryResponse::decode_packet(&packet),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES,
                length: 3,
                available: packet.len()
            })
        );
    }

    fn query_request(file_name: String) -> Smb2QueryDirectoryRequest {
        Smb2QueryDirectoryRequest {
            file_information_class: Smb2FileInformationClass::FILE_DIRECTORY_INFORMATION,
            flags: Smb2QueryDirectoryFlags::default(),
            file_index: 0,
            file_id: Smb2FileId {
                persistent: 1,
                volatile: 2,
            },
            file_name,
            output_buffer_length: 65536,
        }
    }

    fn response_body(output: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(QUERY_DIRECTORY_RESPONSE_FIXED_BYTES);
        push_u16(&mut body, QUERY_DIRECTORY_RESPONSE_STRUCTURE_SIZE);
        push_u16(
            &mut body,
            (SMB2_HEADER_BYTES + QUERY_DIRECTORY_RESPONSE_FIXED_BYTES) as u16,
        );
        push_u32(&mut body, output.len() as u32);
        body
    }
}
