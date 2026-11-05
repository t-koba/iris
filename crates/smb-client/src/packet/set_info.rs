use crate::packet::create::{Smb2FileAttributes, Smb2FileId};
use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{
    check_fixed_body, checked_body_tail_slice, checked_u16_len, checked_u32_len, encode_utf16le,
    le_u16, le_u32, le_u64, push_u16, push_u32, push_u64, response_body,
};
use crate::SmbClientError;

const SET_INFO_REQUEST_STRUCTURE_SIZE: u16 = 33;
const SET_INFO_RESPONSE_STRUCTURE_SIZE: u16 = 2;
const SET_INFO_REQUEST_FIXED_BYTES: usize = 32;
const SET_INFO_RESPONSE_FIXED_BYTES: usize = 2;
const FILE_BASIC_INFORMATION_BYTES: usize = 40;
const FILE_RENAME_INFORMATION_FIXED_BYTES: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Smb2InfoType {
    File = 0x01,
    FileSystem = 0x02,
    Security = 0x03,
    Quota = 0x04,
}

impl Smb2InfoType {
    pub fn from_wire(value: u8) -> Result<Self, SmbClientError> {
        match value {
            0x01 => Ok(Self::File),
            0x02 => Ok(Self::FileSystem),
            0x03 => Ok(Self::Security),
            0x04 => Ok(Self::Quota),
            info_type => Err(SmbClientError::UnknownInfoType { info_type }),
        }
    }

    pub fn to_wire(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2FileInfoClass(pub u8);

impl Smb2FileInfoClass {
    pub const FILE_BASIC_INFORMATION: Self = Self(0x04);
    pub const FILE_RENAME_INFORMATION: Self = Self(0x0a);
    pub const FILE_DISPOSITION_INFORMATION: Self = Self(0x0d);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2SetInfoRequest {
    pub info_type: Smb2InfoType,
    pub file_info_class: Smb2FileInfoClass,
    pub additional_information: u32,
    pub file_id: Smb2FileId,
    pub buffer: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2FileBasicInformation {
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub change_time: u64,
    pub file_attributes: Smb2FileAttributes,
}

impl Smb2SetInfoRequest {
    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let buffer_length = checked_u32_len("set_info.buffer", self.buffer.len())?;
        let buffer_offset = if self.buffer.is_empty() {
            0
        } else {
            checked_u16_len(
                "set_info.buffer_offset",
                SMB2_HEADER_BYTES + SET_INFO_REQUEST_FIXED_BYTES,
            )?
        };
        let mut bytes = Vec::with_capacity(SET_INFO_REQUEST_FIXED_BYTES + self.buffer.len());
        push_u16(&mut bytes, SET_INFO_REQUEST_STRUCTURE_SIZE);
        bytes.push(self.info_type.to_wire());
        bytes.push(self.file_info_class.0);
        push_u32(&mut bytes, buffer_length);
        push_u16(&mut bytes, buffer_offset);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, self.additional_information);
        push_u64(&mut bytes, self.file_id.persistent);
        push_u64(&mut bytes, self.file_id.volatile);
        bytes.extend_from_slice(&self.buffer);
        Ok(bytes)
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        check_fixed_body(
            body,
            SET_INFO_REQUEST_FIXED_BYTES,
            SET_INFO_REQUEST_STRUCTURE_SIZE,
        )?;

        let buffer_length = le_u32(body, 4) as usize;
        let buffer_offset = le_u16(body, 8) as usize;
        let buffer = if buffer_length == 0 {
            Vec::new()
        } else {
            checked_body_tail_slice(
                body,
                buffer_offset,
                buffer_length,
                SET_INFO_REQUEST_FIXED_BYTES,
            )?
            .to_vec()
        };

        Ok(Self {
            info_type: Smb2InfoType::from_wire(body[2])?,
            file_info_class: Smb2FileInfoClass(body[3]),
            additional_information: le_u32(body, 12),
            file_id: Smb2FileId {
                persistent: le_u64(body, 16),
                volatile: le_u64(body, 24),
            },
            buffer,
        })
    }
}

pub fn smb2_file_basic_information(info: Smb2FileBasicInformation) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(FILE_BASIC_INFORMATION_BYTES);
    push_u64(&mut bytes, info.creation_time);
    push_u64(&mut bytes, info.last_access_time);
    push_u64(&mut bytes, info.last_write_time);
    push_u64(&mut bytes, info.change_time);
    push_u32(&mut bytes, info.file_attributes.0);
    push_u32(&mut bytes, 0);
    bytes
}

pub fn smb2_file_disposition_information(delete_pending: bool) -> Vec<u8> {
    vec![u8::from(delete_pending)]
}

pub fn smb2_file_rename_information(
    replace_if_exists: bool,
    file_name: &str,
) -> Result<Vec<u8>, SmbClientError> {
    let file_name_bytes = encode_utf16le(file_name);
    let file_name_length =
        checked_u32_len("file_rename_information.file_name", file_name_bytes.len())?;
    let mut bytes = Vec::with_capacity(FILE_RENAME_INFORMATION_FIXED_BYTES + file_name_bytes.len());
    bytes.push(u8::from(replace_if_exists));
    bytes.extend_from_slice(&[0; 7]);
    push_u64(&mut bytes, 0);
    push_u32(&mut bytes, file_name_length);
    bytes.extend_from_slice(&file_name_bytes);
    Ok(bytes)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2SetInfoResponse;

impl Smb2SetInfoResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        response_body(
            packet,
            SET_INFO_RESPONSE_FIXED_BYTES,
            SET_INFO_RESPONSE_STRUCTURE_SIZE,
        )?;
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn set_info_request_round_trips_buffer() {
        let request = Smb2SetInfoRequest {
            info_type: Smb2InfoType::File,
            file_info_class: Smb2FileInfoClass::FILE_RENAME_INFORMATION,
            additional_information: 0x0102_0304,
            file_id: Smb2FileId {
                persistent: 0x0102_0304_0506_0708,
                volatile: 0x1112_1314_1516_1718,
            },
            buffer: b"rename-info".to_vec(),
        };

        let encoded = request.encode().expect("encode");
        let decoded = Smb2SetInfoRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), SET_INFO_REQUEST_STRUCTURE_SIZE);
        assert_eq!(
            le_u16(&encoded, 8),
            (SMB2_HEADER_BYTES + SET_INFO_REQUEST_FIXED_BYTES) as u16
        );
    }

    #[test]
    fn set_info_request_rejects_unknown_info_type() {
        let mut encoded = empty_set_info_request().encode().expect("encode");
        encoded[2] = 0xff;

        assert_eq!(
            Smb2SetInfoRequest::decode(&encoded),
            Err(SmbClientError::UnknownInfoType { info_type: 0xff })
        );
    }

    #[test]
    fn set_info_request_rejects_buffer_inside_fixed_body() {
        let mut encoded = empty_set_info_request().encode().expect("encode");
        encoded[4..8].copy_from_slice(&4u32.to_le_bytes());
        encoded[8..10].copy_from_slice(&(SMB2_HEADER_BYTES as u16 + 1).to_le_bytes());

        assert_eq!(
            Smb2SetInfoRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + 1,
                length: 4,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn set_info_response_decodes_structure_size() {
        let mut packet = Smb2Header::new(Smb2Command::SetInfo, 12).encode().to_vec();
        packet.extend_from_slice(&SET_INFO_RESPONSE_STRUCTURE_SIZE.to_le_bytes());

        let response = Smb2SetInfoResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response, Smb2SetInfoResponse);
    }

    #[test]
    fn set_info_response_rejects_bad_structure_size() {
        let mut packet = Smb2Header::new(Smb2Command::SetInfo, 12).encode().to_vec();
        packet.extend_from_slice(&3u16.to_le_bytes());

        assert_eq!(
            Smb2SetInfoResponse::decode_packet(&packet),
            Err(SmbClientError::InvalidStructureSize { size: 3 })
        );
    }

    #[test]
    fn file_disposition_information_encodes_delete_pending_flag() {
        assert_eq!(smb2_file_disposition_information(true), vec![1]);
        assert_eq!(smb2_file_disposition_information(false), vec![0]);
    }

    #[test]
    fn file_basic_information_encodes_times_attributes_and_reserved_bytes() {
        let encoded = smb2_file_basic_information(Smb2FileBasicInformation {
            creation_time: 1,
            last_access_time: 2,
            last_write_time: 3,
            change_time: 4,
            file_attributes: Smb2FileAttributes::ARCHIVE,
        });

        assert_eq!(encoded.len(), FILE_BASIC_INFORMATION_BYTES);
        assert_eq!(le_u64(&encoded, 0), 1);
        assert_eq!(le_u64(&encoded, 8), 2);
        assert_eq!(le_u64(&encoded, 16), 3);
        assert_eq!(le_u64(&encoded, 24), 4);
        assert_eq!(le_u32(&encoded, 32), Smb2FileAttributes::ARCHIVE.0);
        assert_eq!(le_u32(&encoded, 36), 0);
    }

    #[test]
    fn file_rename_information_encodes_replace_root_and_utf16_name() {
        let encoded = smb2_file_rename_information(true, "folder\\renamed.txt").expect("encode");

        assert_eq!(encoded[0], 1);
        assert_eq!(&encoded[1..8], &[0; 7]);
        assert_eq!(le_u64(&encoded, 8), 0);
        let file_name = encode_utf16le("folder\\renamed.txt");
        assert_eq!(le_u32(&encoded, 16), file_name.len() as u32);
        assert_eq!(&encoded[20..], file_name.as_slice());
    }

    fn empty_set_info_request() -> Smb2SetInfoRequest {
        Smb2SetInfoRequest {
            info_type: Smb2InfoType::File,
            file_info_class: Smb2FileInfoClass::FILE_BASIC_INFORMATION,
            additional_information: 0,
            file_id: Smb2FileId {
                persistent: 1,
                volatile: 2,
            },
            buffer: Vec::new(),
        }
    }
}
