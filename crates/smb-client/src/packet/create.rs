use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{
    align8, check_fixed_body, checked_slice, checked_u16_len, checked_u32_len, decode_utf16le,
    encode_utf16le, le_u16, le_u32, le_u64, push_u16, push_u32, push_u64, put_u32, response_body,
};
use crate::SmbClientError;

const CREATE_REQUEST_STRUCTURE_SIZE: u16 = 57;
const CREATE_RESPONSE_STRUCTURE_SIZE: u16 = 89;
const CREATE_REQUEST_FIXED_BYTES: usize = 56;
const CREATE_RESPONSE_FIXED_BYTES: usize = 88;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Smb2OplockLevel {
    None = 0x00,
    Level2 = 0x01,
    Exclusive = 0x08,
    Batch = 0x09,
    Lease = 0xff,
}

impl Smb2OplockLevel {
    pub fn from_wire(value: u8) -> Result<Self, SmbClientError> {
        match value {
            0x00 => Ok(Self::None),
            0x01 => Ok(Self::Level2),
            0x08 => Ok(Self::Exclusive),
            0x09 => Ok(Self::Batch),
            0xff => Ok(Self::Lease),
            level => Err(SmbClientError::UnknownOplockLevel { level }),
        }
    }

    pub fn to_wire(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Smb2ImpersonationLevel {
    Anonymous = 0x0000_0000,
    Identification = 0x0000_0001,
    Impersonation = 0x0000_0002,
    Delegate = 0x0000_0003,
}

impl Smb2ImpersonationLevel {
    pub fn from_wire(value: u32) -> Result<Self, SmbClientError> {
        match value {
            0x0000_0000 => Ok(Self::Anonymous),
            0x0000_0001 => Ok(Self::Identification),
            0x0000_0002 => Ok(Self::Impersonation),
            0x0000_0003 => Ok(Self::Delegate),
            level => Err(SmbClientError::UnknownImpersonationLevel { level }),
        }
    }

    pub fn to_wire(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2DesiredAccess(pub u32);

impl Smb2DesiredAccess {
    pub const FILE_READ_DATA: Self = Self(0x0000_0001);
    pub const FILE_WRITE_DATA: Self = Self(0x0000_0002);
    pub const FILE_READ_EA: Self = Self(0x0000_0008);
    pub const FILE_READ_ATTRIBUTES: Self = Self(0x0000_0080);
    pub const FILE_WRITE_ATTRIBUTES: Self = Self(0x0000_0100);
    pub const DELETE: Self = Self(0x0001_0000);
    pub const SYNCHRONIZE: Self = Self(0x0010_0000);
    pub const GENERIC_WRITE: Self = Self(0x4000_0000);
    pub const GENERIC_READ: Self = Self(0x8000_0000);

    pub fn contains(self, access: Self) -> bool {
        self.0 & access.0 == access.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2FileAttributes(pub u32);

impl Smb2FileAttributes {
    pub const DIRECTORY: Self = Self(0x0000_0010);
    pub const ARCHIVE: Self = Self(0x0000_0020);
    pub const NORMAL: Self = Self(0x0000_0080);

    pub fn contains(self, attributes: Self) -> bool {
        self.0 & attributes.0 == attributes.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2ShareAccess(pub u32);

impl Smb2ShareAccess {
    pub const READ: Self = Self(0x0000_0001);
    pub const WRITE: Self = Self(0x0000_0002);
    pub const DELETE: Self = Self(0x0000_0004);

    pub fn contains(self, access: Self) -> bool {
        self.0 & access.0 == access.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Smb2CreateDisposition {
    Supersede = 0x0000_0000,
    Open = 0x0000_0001,
    Create = 0x0000_0002,
    OpenIf = 0x0000_0003,
    Overwrite = 0x0000_0004,
    OverwriteIf = 0x0000_0005,
}

impl Smb2CreateDisposition {
    pub fn from_wire(value: u32) -> Result<Self, SmbClientError> {
        match value {
            0x0000_0000 => Ok(Self::Supersede),
            0x0000_0001 => Ok(Self::Open),
            0x0000_0002 => Ok(Self::Create),
            0x0000_0003 => Ok(Self::OpenIf),
            0x0000_0004 => Ok(Self::Overwrite),
            0x0000_0005 => Ok(Self::OverwriteIf),
            disposition => Err(SmbClientError::UnknownCreateDisposition { disposition }),
        }
    }

    pub fn to_wire(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2CreateOptions(pub u32);

impl Smb2CreateOptions {
    pub const DIRECTORY_FILE: Self = Self(0x0000_0001);
    pub const SEQUENTIAL_ONLY: Self = Self(0x0000_0004);
    pub const NON_DIRECTORY_FILE: Self = Self(0x0000_0040);

    pub fn contains(self, options: Self) -> bool {
        self.0 & options.0 == options.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Smb2CreateAction {
    Superseded = 0x0000_0000,
    Opened = 0x0000_0001,
    Created = 0x0000_0002,
    Overwritten = 0x0000_0003,
}

impl Smb2CreateAction {
    pub fn from_wire(value: u32) -> Result<Self, SmbClientError> {
        match value {
            0x0000_0000 => Ok(Self::Superseded),
            0x0000_0001 => Ok(Self::Opened),
            0x0000_0002 => Ok(Self::Created),
            0x0000_0003 => Ok(Self::Overwritten),
            action => Err(SmbClientError::UnknownCreateAction { action }),
        }
    }

    pub fn to_wire(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2FileId {
    pub persistent: u64,
    pub volatile: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2CreateRequest {
    pub security_flags: u8,
    pub requested_oplock_level: Smb2OplockLevel,
    pub impersonation_level: Smb2ImpersonationLevel,
    pub smb_create_flags: u64,
    pub desired_access: Smb2DesiredAccess,
    pub file_attributes: Smb2FileAttributes,
    pub share_access: Smb2ShareAccess,
    pub create_disposition: Smb2CreateDisposition,
    pub create_options: Smb2CreateOptions,
    pub name: String,
    pub create_contexts: Vec<u8>,
}

impl Smb2CreateRequest {
    /// A request without oplocks or create contexts that shares read, write and delete access.
    pub fn new(
        name: impl Into<String>,
        desired_access: u32,
        file_attributes: Smb2FileAttributes,
        create_disposition: Smb2CreateDisposition,
        create_options: Smb2CreateOptions,
    ) -> Self {
        Self {
            security_flags: 0,
            requested_oplock_level: Smb2OplockLevel::None,
            impersonation_level: Smb2ImpersonationLevel::Impersonation,
            smb_create_flags: 0,
            desired_access: Smb2DesiredAccess(desired_access),
            file_attributes,
            share_access: Smb2ShareAccess(
                Smb2ShareAccess::READ.0 | Smb2ShareAccess::WRITE.0 | Smb2ShareAccess::DELETE.0,
            ),
            create_disposition,
            create_options,
            name: name.into(),
            create_contexts: Vec::new(),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let name_bytes = encode_utf16le(&self.name);
        let name_length = checked_u16_len("create.name", name_bytes.len())?;
        let contexts_length = checked_u32_len("create.contexts", self.create_contexts.len())?;
        let name_offset = (SMB2_HEADER_BYTES + CREATE_REQUEST_FIXED_BYTES) as u16;

        let mut bytes = Vec::with_capacity(
            CREATE_REQUEST_FIXED_BYTES + name_bytes.len() + self.create_contexts.len(),
        );
        push_u16(&mut bytes, CREATE_REQUEST_STRUCTURE_SIZE);
        bytes.push(self.security_flags);
        bytes.push(self.requested_oplock_level.to_wire());
        push_u32(&mut bytes, self.impersonation_level.to_wire());
        push_u64(&mut bytes, self.smb_create_flags);
        push_u64(&mut bytes, 0);
        push_u32(&mut bytes, self.desired_access.0);
        push_u32(&mut bytes, self.file_attributes.0);
        push_u32(&mut bytes, self.share_access.0);
        push_u32(&mut bytes, self.create_disposition.to_wire());
        push_u32(&mut bytes, self.create_options.0);
        push_u16(&mut bytes, name_offset);
        push_u16(&mut bytes, name_length);
        let contexts_offset_position = bytes.len();
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, contexts_length);
        bytes.extend_from_slice(&name_bytes);

        if !self.create_contexts.is_empty() {
            let next_absolute_offset = SMB2_HEADER_BYTES + bytes.len();
            let padding = align8(next_absolute_offset) - next_absolute_offset;
            bytes.resize(bytes.len() + padding, 0);
            let contexts_offset =
                checked_u32_len("create.contexts_offset", SMB2_HEADER_BYTES + bytes.len())?;
            put_u32(&mut bytes, contexts_offset_position, contexts_offset);
            bytes.extend_from_slice(&self.create_contexts);
        }

        Ok(bytes)
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        check_fixed_body(
            body,
            CREATE_REQUEST_FIXED_BYTES,
            CREATE_REQUEST_STRUCTURE_SIZE,
        )?;

        let name_offset = le_u16(body, 44) as usize;
        let name_length = le_u16(body, 46) as usize;
        let name = if name_length == 0 {
            String::new()
        } else {
            let body_name_offset = name_offset.checked_sub(SMB2_HEADER_BYTES).ok_or(
                SmbClientError::BufferRangeOutOfBounds {
                    offset: name_offset,
                    length: name_length,
                    available: SMB2_HEADER_BYTES + body.len(),
                },
            )?;
            decode_utf16le(checked_slice(body, body_name_offset, name_length)?)?
        };

        let contexts_offset = le_u32(body, 48) as usize;
        let contexts_length = le_u32(body, 52) as usize;
        let create_contexts = if contexts_length == 0 {
            Vec::new()
        } else {
            let body_contexts_offset = contexts_offset.checked_sub(SMB2_HEADER_BYTES).ok_or(
                SmbClientError::BufferRangeOutOfBounds {
                    offset: contexts_offset,
                    length: contexts_length,
                    available: SMB2_HEADER_BYTES + body.len(),
                },
            )?;
            checked_slice(body, body_contexts_offset, contexts_length)?.to_vec()
        };

        Ok(Self {
            security_flags: body[2],
            requested_oplock_level: Smb2OplockLevel::from_wire(body[3])?,
            impersonation_level: Smb2ImpersonationLevel::from_wire(le_u32(body, 4))?,
            smb_create_flags: le_u64(body, 8),
            desired_access: Smb2DesiredAccess(le_u32(body, 24)),
            file_attributes: Smb2FileAttributes(le_u32(body, 28)),
            share_access: Smb2ShareAccess(le_u32(body, 32)),
            create_disposition: Smb2CreateDisposition::from_wire(le_u32(body, 36))?,
            create_options: Smb2CreateOptions(le_u32(body, 40)),
            name,
            create_contexts,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2CreateResponse {
    pub oplock_level: Smb2OplockLevel,
    pub flags: u8,
    pub create_action: Smb2CreateAction,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub change_time: u64,
    pub allocation_size: u64,
    pub end_of_file: u64,
    pub file_attributes: Smb2FileAttributes,
    pub file_id: Smb2FileId,
    pub create_contexts: Vec<u8>,
}

impl Smb2CreateResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body = response_body(
            packet,
            CREATE_RESPONSE_FIXED_BYTES,
            CREATE_RESPONSE_STRUCTURE_SIZE,
        )?;

        let create_contexts_offset = le_u32(body, 80) as usize;
        let create_contexts_length = le_u32(body, 84) as usize;
        let create_contexts = if create_contexts_length == 0 {
            Vec::new()
        } else {
            checked_slice(packet, create_contexts_offset, create_contexts_length)?.to_vec()
        };

        Ok(Self {
            oplock_level: Smb2OplockLevel::from_wire(body[2])?,
            flags: body[3],
            create_action: Smb2CreateAction::from_wire(le_u32(body, 4))?,
            creation_time: le_u64(body, 8),
            last_access_time: le_u64(body, 16),
            last_write_time: le_u64(body, 24),
            change_time: le_u64(body, 32),
            allocation_size: le_u64(body, 40),
            end_of_file: le_u64(body, 48),
            file_attributes: Smb2FileAttributes(le_u32(body, 56)),
            file_id: Smb2FileId {
                persistent: le_u64(body, 64),
                volatile: le_u64(body, 72),
            },
            create_contexts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn create_request_round_trips_name_and_contexts() {
        let request = Smb2CreateRequest {
            security_flags: 0,
            requested_oplock_level: Smb2OplockLevel::Batch,
            impersonation_level: Smb2ImpersonationLevel::Impersonation,
            smb_create_flags: 0,
            desired_access: Smb2DesiredAccess::GENERIC_READ,
            file_attributes: Smb2FileAttributes::NORMAL,
            share_access: Smb2ShareAccess(
                Smb2ShareAccess::READ.0 | Smb2ShareAccess::WRITE.0 | Smb2ShareAccess::DELETE.0,
            ),
            create_disposition: Smb2CreateDisposition::Open,
            create_options: Smb2CreateOptions::NON_DIRECTORY_FILE,
            name: "dir\\ファイル.txt".to_string(),
            create_contexts: b"context".to_vec(),
        };

        let encoded = request.encode().expect("encode");
        let decoded = Smb2CreateRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), CREATE_REQUEST_STRUCTURE_SIZE);
        assert_eq!(
            le_u16(&encoded, 44),
            (SMB2_HEADER_BYTES + CREATE_REQUEST_FIXED_BYTES) as u16
        );
        assert_eq!(le_u32(&encoded, 48) as usize % 8, 0);
    }

    #[test]
    fn create_request_encodes_root_name_with_buffer_offset() {
        let request = Smb2CreateRequest {
            security_flags: 0,
            requested_oplock_level: Smb2OplockLevel::None,
            impersonation_level: Smb2ImpersonationLevel::Impersonation,
            smb_create_flags: 0,
            desired_access: Smb2DesiredAccess::GENERIC_READ,
            file_attributes: Smb2FileAttributes::DIRECTORY,
            share_access: Smb2ShareAccess::READ,
            create_disposition: Smb2CreateDisposition::Open,
            create_options: Smb2CreateOptions::DIRECTORY_FILE,
            name: String::new(),
            create_contexts: Vec::new(),
        };

        let encoded = request.encode().expect("encode");
        let decoded = Smb2CreateRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(
            le_u16(&encoded, 44),
            (SMB2_HEADER_BYTES + CREATE_REQUEST_FIXED_BYTES) as u16
        );
        assert_eq!(le_u16(&encoded, 46), 0);
        assert_eq!(le_u32(&encoded, 48), 0);
        assert_eq!(le_u32(&encoded, 52), 0);
    }

    #[test]
    fn create_request_rejects_name_before_body() {
        let request = create_request("file.txt");
        let mut encoded = request.encode().expect("encode");
        encoded[44..46].copy_from_slice(&(SMB2_HEADER_BYTES as u16 - 1).to_le_bytes());

        assert_eq!(
            Smb2CreateRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES - 1,
                length: 16,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn create_request_rejects_odd_utf16_name_length() {
        let request = create_request("file.txt");
        let mut encoded = request.encode().expect("encode");
        encoded[46..48].copy_from_slice(&15u16.to_le_bytes());

        assert_eq!(
            Smb2CreateRequest::decode(&encoded),
            Err(SmbClientError::InvalidUtf16Length { length: 15 })
        );
    }

    #[test]
    fn create_request_rejects_too_large_name() {
        let request = create_request(&"x".repeat(32768));

        assert_eq!(
            request.encode(),
            Err(SmbClientError::FieldTooLarge {
                field: "create.name",
                length: 65536,
                max: u16::MAX as usize
            })
        );
    }

    #[test]
    fn create_request_rejects_unknown_impersonation_level() {
        let request = create_request("file.txt");
        let mut encoded = request.encode().expect("encode");
        encoded[4..8].copy_from_slice(&0xffff_ffffu32.to_le_bytes());

        assert_eq!(
            Smb2CreateRequest::decode(&encoded),
            Err(SmbClientError::UnknownImpersonationLevel { level: 0xffff_ffff })
        );
    }

    #[test]
    fn create_request_rejects_unknown_create_disposition() {
        let request = create_request("file.txt");
        let mut encoded = request.encode().expect("encode");
        encoded[36..40].copy_from_slice(&0xffff_ffffu32.to_le_bytes());

        assert_eq!(
            Smb2CreateRequest::decode(&encoded),
            Err(SmbClientError::UnknownCreateDisposition {
                disposition: 0xffff_ffff
            })
        );
    }

    #[test]
    fn create_response_decodes_handle_and_contexts() {
        let mut packet = Smb2Header::new(Smb2Command::Create, 12).encode().to_vec();
        let contexts = b"leasectx";
        packet.extend_from_slice(&response_body(contexts));
        packet.extend_from_slice(contexts);

        let response = Smb2CreateResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response.oplock_level, Smb2OplockLevel::Lease);
        assert_eq!(response.flags, 0x01);
        assert_eq!(response.create_action, Smb2CreateAction::Opened);
        assert_eq!(response.creation_time, 1);
        assert_eq!(response.last_access_time, 2);
        assert_eq!(response.last_write_time, 3);
        assert_eq!(response.change_time, 4);
        assert_eq!(response.allocation_size, 4096);
        assert_eq!(response.end_of_file, 128);
        assert_eq!(response.file_attributes, Smb2FileAttributes::ARCHIVE);
        assert_eq!(
            response.file_id,
            Smb2FileId {
                persistent: 0x1111_2222_3333_4444,
                volatile: 0x5555_6666_7777_8888
            }
        );
        assert_eq!(response.create_contexts, contexts);
    }

    #[test]
    fn create_response_rejects_unknown_oplock_level() {
        let mut packet = Smb2Header::new(Smb2Command::Create, 12).encode().to_vec();
        packet.extend_from_slice(&response_body(&[]));
        packet[SMB2_HEADER_BYTES + 2] = 0xee;

        assert_eq!(
            Smb2CreateResponse::decode_packet(&packet),
            Err(SmbClientError::UnknownOplockLevel { level: 0xee })
        );
    }

    #[test]
    fn create_response_rejects_unknown_create_action() {
        let mut packet = Smb2Header::new(Smb2Command::Create, 12).encode().to_vec();
        packet.extend_from_slice(&response_body(&[]));
        packet[SMB2_HEADER_BYTES + 4..SMB2_HEADER_BYTES + 8]
            .copy_from_slice(&0xffff_ffffu32.to_le_bytes());

        assert_eq!(
            Smb2CreateResponse::decode_packet(&packet),
            Err(SmbClientError::UnknownCreateAction {
                action: 0xffff_ffff
            })
        );
    }

    #[test]
    fn create_response_rejects_contexts_outside_packet() {
        let mut packet = Smb2Header::new(Smb2Command::Create, 12).encode().to_vec();
        packet.extend_from_slice(&response_body(b"ctx"));
        let body_offset = SMB2_HEADER_BYTES;
        packet[body_offset + 84..body_offset + 88].copy_from_slice(&8u32.to_le_bytes());

        assert_eq!(
            Smb2CreateResponse::decode_packet(&packet),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + CREATE_RESPONSE_FIXED_BYTES,
                length: 8,
                available: packet.len()
            })
        );
    }

    fn create_request(name: &str) -> Smb2CreateRequest {
        Smb2CreateRequest {
            security_flags: 0,
            requested_oplock_level: Smb2OplockLevel::None,
            impersonation_level: Smb2ImpersonationLevel::Impersonation,
            smb_create_flags: 0,
            desired_access: Smb2DesiredAccess::GENERIC_READ,
            file_attributes: Smb2FileAttributes::NORMAL,
            share_access: Smb2ShareAccess::READ,
            create_disposition: Smb2CreateDisposition::Open,
            create_options: Smb2CreateOptions::NON_DIRECTORY_FILE,
            name: name.to_string(),
            create_contexts: Vec::new(),
        }
    }

    fn response_body(contexts: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(CREATE_RESPONSE_FIXED_BYTES);
        push_u16(&mut body, CREATE_RESPONSE_STRUCTURE_SIZE);
        body.push(Smb2OplockLevel::Lease.to_wire());
        body.push(0x01);
        push_u32(&mut body, Smb2CreateAction::Opened.to_wire());
        push_u64(&mut body, 1);
        push_u64(&mut body, 2);
        push_u64(&mut body, 3);
        push_u64(&mut body, 4);
        push_u64(&mut body, 4096);
        push_u64(&mut body, 128);
        push_u32(&mut body, Smb2FileAttributes::ARCHIVE.0);
        push_u32(&mut body, 0);
        push_u64(&mut body, 0x1111_2222_3333_4444);
        push_u64(&mut body, 0x5555_6666_7777_8888);
        if contexts.is_empty() {
            push_u32(&mut body, 0);
        } else {
            push_u32(
                &mut body,
                (SMB2_HEADER_BYTES + CREATE_RESPONSE_FIXED_BYTES) as u32,
            );
        }
        push_u32(&mut body, contexts.len() as u32);
        body
    }
}
