use crate::wire::{le_u16, le_u32, le_u64};
use crate::SmbClientError;

pub const SMB2_HEADER_BYTES: usize = 64;
pub const SMB2_FLAGS_SIGNED: u32 = 0x0000_0008;
pub const SMB2_FLAGS_RELATED_OPERATIONS: u32 = 0x0000_0004;
pub const SMB2_CREDIT_UNIT_BYTES: usize = 64 * 1024;
const SMB2_PROTOCOL_ID: [u8; 4] = [0xfe, b'S', b'M', b'B'];
const SMB2_STRUCTURE_SIZE: u16 = 64;
const SMB2_MAX_CREDIT_CHARGE_PAYLOAD_BYTES: usize = SMB2_CREDIT_UNIT_BYTES * u16::MAX as usize;

pub fn smb2_credit_charge_for_payload(payload_bytes: usize) -> Result<u16, SmbClientError> {
    if payload_bytes > SMB2_MAX_CREDIT_CHARGE_PAYLOAD_BYTES {
        return Err(SmbClientError::FieldTooLarge {
            field: "credit_charge.payload",
            length: payload_bytes,
            max: SMB2_MAX_CREDIT_CHARGE_PAYLOAD_BYTES,
        });
    }
    let charge = payload_bytes.div_ceil(SMB2_CREDIT_UNIT_BYTES).max(1);
    Ok(charge as u16)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SmbNtStatus(pub u32);

impl SmbNtStatus {
    pub const SUCCESS: Self = Self(0x0000_0000);
    pub const NO_MORE_FILES: Self = Self(0x8000_0006);
    pub const MORE_PROCESSING_REQUIRED: Self = Self(0xc000_0016);
    pub const END_OF_FILE: Self = Self(0xc000_0011);

    pub fn is_success(self) -> bool {
        self.0 == Self::SUCCESS.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum Smb2Command {
    Negotiate = 0x0000,
    SessionSetup = 0x0001,
    Logoff = 0x0002,
    TreeConnect = 0x0003,
    TreeDisconnect = 0x0004,
    Create = 0x0005,
    Close = 0x0006,
    Flush = 0x0007,
    Read = 0x0008,
    Write = 0x0009,
    Lock = 0x000a,
    Ioctl = 0x000b,
    Cancel = 0x000c,
    Echo = 0x000d,
    QueryDirectory = 0x000e,
    ChangeNotify = 0x000f,
    QueryInfo = 0x0010,
    SetInfo = 0x0011,
    OplockBreak = 0x0012,
}

impl Smb2Command {
    pub fn from_wire(value: u16) -> Result<Self, SmbClientError> {
        match value {
            0x0000 => Ok(Self::Negotiate),
            0x0001 => Ok(Self::SessionSetup),
            0x0002 => Ok(Self::Logoff),
            0x0003 => Ok(Self::TreeConnect),
            0x0004 => Ok(Self::TreeDisconnect),
            0x0005 => Ok(Self::Create),
            0x0006 => Ok(Self::Close),
            0x0007 => Ok(Self::Flush),
            0x0008 => Ok(Self::Read),
            0x0009 => Ok(Self::Write),
            0x000a => Ok(Self::Lock),
            0x000b => Ok(Self::Ioctl),
            0x000c => Ok(Self::Cancel),
            0x000d => Ok(Self::Echo),
            0x000e => Ok(Self::QueryDirectory),
            0x000f => Ok(Self::ChangeNotify),
            0x0010 => Ok(Self::QueryInfo),
            0x0011 => Ok(Self::SetInfo),
            0x0012 => Ok(Self::OplockBreak),
            command => Err(SmbClientError::UnknownCommand { command }),
        }
    }

    pub fn to_wire(self) -> u16 {
        self as u16
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2Header {
    pub credit_charge: u16,
    pub status_or_channel_sequence: u32,
    pub command: Smb2Command,
    pub credit_request_response: u16,
    pub flags: u32,
    pub next_command: u32,
    pub message_id: u64,
    pub tree_id: u32,
    pub session_id: u64,
    pub signature: [u8; 16],
}

impl Smb2Header {
    pub fn new(command: Smb2Command, message_id: u64) -> Self {
        Self {
            credit_charge: 0,
            status_or_channel_sequence: 0,
            command,
            credit_request_response: 1,
            flags: 0,
            next_command: 0,
            message_id,
            tree_id: 0,
            session_id: 0,
            signature: [0; 16],
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SmbClientError> {
        if bytes.len() < SMB2_HEADER_BYTES {
            return Err(SmbClientError::ShortFrame {
                expected: SMB2_HEADER_BYTES,
                actual: bytes.len(),
            });
        }
        if bytes[0..4] != SMB2_PROTOCOL_ID {
            return Err(SmbClientError::InvalidProtocolId);
        }

        let structure_size = le_u16(bytes, 4);
        if structure_size != SMB2_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        let mut signature = [0; 16];
        signature.copy_from_slice(&bytes[48..64]);

        Ok(Self {
            credit_charge: le_u16(bytes, 6),
            status_or_channel_sequence: le_u32(bytes, 8),
            command: Smb2Command::from_wire(le_u16(bytes, 12))?,
            credit_request_response: le_u16(bytes, 14),
            flags: le_u32(bytes, 16),
            next_command: le_u32(bytes, 20),
            message_id: le_u64(bytes, 24),
            tree_id: le_u32(bytes, 36),
            session_id: le_u64(bytes, 40),
            signature,
        })
    }

    pub fn encode(&self) -> [u8; SMB2_HEADER_BYTES] {
        let mut bytes = [0; SMB2_HEADER_BYTES];
        bytes[0..4].copy_from_slice(&SMB2_PROTOCOL_ID);
        put_le_u16(&mut bytes, 4, SMB2_STRUCTURE_SIZE);
        put_le_u16(&mut bytes, 6, self.credit_charge);
        put_le_u32(&mut bytes, 8, self.status_or_channel_sequence);
        put_le_u16(&mut bytes, 12, self.command.to_wire());
        put_le_u16(&mut bytes, 14, self.credit_request_response);
        put_le_u32(&mut bytes, 16, self.flags);
        put_le_u32(&mut bytes, 20, self.next_command);
        put_le_u64(&mut bytes, 24, self.message_id);
        put_le_u32(&mut bytes, 36, self.tree_id);
        put_le_u64(&mut bytes, 40, self.session_id);
        bytes[48..64].copy_from_slice(&self.signature);
        bytes
    }

    pub fn status(&self) -> SmbNtStatus {
        SmbNtStatus(self.status_or_channel_sequence)
    }
}

fn put_le_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_le_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_le_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn credit_charge_is_the_ceiling_number_of_64k_units(length in 0usize..=16 * 1024 * 1024) {
            let charge = smb2_credit_charge_for_payload(length).expect("supported payload");
            let expected = length.max(1).div_ceil(SMB2_CREDIT_UNIT_BYTES) as u16;
            prop_assert_eq!(charge, expected);
        }
    }

    #[test]
    fn smb2_header_round_trips_wire_values() {
        let mut header = Smb2Header::new(Smb2Command::Read, 0x0102_0304_0506_0708);
        header.credit_charge = 16;
        header.status_or_channel_sequence = SmbNtStatus::MORE_PROCESSING_REQUIRED.0;
        header.credit_request_response = 32;
        header.flags = 0x0000_0008;
        header.next_command = 128;
        header.tree_id = 0x2122_2324;
        header.session_id = 0x3132_3334_3536_3738;
        header.signature = [0xab; 16];

        let encoded = header.encode();
        let decoded = Smb2Header::decode(&encoded).expect("decode");

        assert_eq!(decoded, header);
        assert_eq!(decoded.status(), SmbNtStatus::MORE_PROCESSING_REQUIRED);
        assert!(!decoded.status().is_success());
        assert_eq!(encoded[0..4], SMB2_PROTOCOL_ID);
        assert_eq!(le_u16(&encoded, 4), SMB2_STRUCTURE_SIZE);
    }

    #[test]
    fn smb2_header_rejects_short_frame() {
        let error = Smb2Header::decode(&[0; SMB2_HEADER_BYTES - 1]).expect_err("reject");

        assert_eq!(
            error,
            SmbClientError::ShortFrame {
                expected: SMB2_HEADER_BYTES,
                actual: SMB2_HEADER_BYTES - 1
            }
        );
    }

    #[test]
    fn smb2_header_rejects_bad_protocol_id() {
        let mut bytes = Smb2Header::new(Smb2Command::Negotiate, 0).encode();
        bytes[0] = 0xff;

        assert_eq!(
            Smb2Header::decode(&bytes),
            Err(SmbClientError::InvalidProtocolId)
        );
    }

    #[test]
    fn smb2_header_rejects_bad_structure_size() {
        let mut bytes = Smb2Header::new(Smb2Command::Negotiate, 0).encode();
        put_le_u16(&mut bytes, 4, 65);

        assert_eq!(
            Smb2Header::decode(&bytes),
            Err(SmbClientError::InvalidStructureSize { size: 65 })
        );
    }

    #[test]
    fn smb2_header_rejects_unknown_command() {
        let mut bytes = Smb2Header::new(Smb2Command::Negotiate, 0).encode();
        put_le_u16(&mut bytes, 12, 0xffff);

        assert_eq!(
            Smb2Header::decode(&bytes),
            Err(SmbClientError::UnknownCommand { command: 0xffff })
        );
    }

    #[test]
    fn credit_charge_counts_64k_payload_units() {
        assert_eq!(smb2_credit_charge_for_payload(0), Ok(1));
        assert_eq!(smb2_credit_charge_for_payload(1), Ok(1));
        assert_eq!(
            smb2_credit_charge_for_payload(SMB2_CREDIT_UNIT_BYTES),
            Ok(1)
        );
        assert_eq!(
            smb2_credit_charge_for_payload(SMB2_CREDIT_UNIT_BYTES + 1),
            Ok(2)
        );
        assert_eq!(smb2_credit_charge_for_payload(1024 * 1024), Ok(16));
    }

    #[test]
    fn credit_charge_rejects_payload_above_header_capacity() {
        assert_eq!(
            smb2_credit_charge_for_payload(SMB2_MAX_CREDIT_CHARGE_PAYLOAD_BYTES + 1),
            Err(SmbClientError::FieldTooLarge {
                field: "credit_charge.payload",
                length: SMB2_MAX_CREDIT_CHARGE_PAYLOAD_BYTES + 1,
                max: SMB2_MAX_CREDIT_CHARGE_PAYLOAD_BYTES
            })
        );
    }
}
