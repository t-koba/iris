use crate::packet::create::Smb2FileId;
use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{
    check_fixed_body, checked_body_tail_slice, checked_packet_tail_slice, checked_u16_len, le_u16,
    le_u32, le_u64, push_u16, push_u32, push_u64, response_body,
};
use crate::SmbClientError;

const READ_REQUEST_STRUCTURE_SIZE: u16 = 49;
const READ_RESPONSE_STRUCTURE_SIZE: u16 = 17;
const READ_REQUEST_FIXED_BYTES: usize = 48;
const READ_RESPONSE_FIXED_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2ReadFlags(pub u8);

impl Smb2ReadFlags {
    pub const READ_UNBUFFERED: Self = Self(0x01);

    pub fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2ReadChannel(pub u32);

impl Smb2ReadChannel {
    pub const NONE: Self = Self(0x0000_0000);
    pub const RDMA_V1: Self = Self(0x0000_0001);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2ReadRequest {
    pub flags: Smb2ReadFlags,
    pub length: u32,
    pub offset: u64,
    pub file_id: Smb2FileId,
    pub minimum_count: u32,
    pub channel: Smb2ReadChannel,
    pub remaining_bytes: u32,
    pub channel_info: Vec<u8>,
}

impl Smb2ReadRequest {
    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let channel_info_length = checked_u16_len("read.channel_info", self.channel_info.len())?;
        let mut bytes = Vec::with_capacity(READ_REQUEST_FIXED_BYTES + self.channel_info.len());
        push_u16(&mut bytes, READ_REQUEST_STRUCTURE_SIZE);
        bytes.push(0);
        bytes.push(self.flags.0);
        push_u32(&mut bytes, self.length);
        push_u64(&mut bytes, self.offset);
        push_u64(&mut bytes, self.file_id.persistent);
        push_u64(&mut bytes, self.file_id.volatile);
        push_u32(&mut bytes, self.minimum_count);
        push_u32(&mut bytes, self.channel.0);
        push_u32(&mut bytes, self.remaining_bytes);

        if self.channel_info.is_empty() {
            push_u16(&mut bytes, 0);
            push_u16(&mut bytes, 0);
        } else {
            push_u16(
                &mut bytes,
                (SMB2_HEADER_BYTES + READ_REQUEST_FIXED_BYTES) as u16,
            );
            push_u16(&mut bytes, channel_info_length);
            bytes.extend_from_slice(&self.channel_info);
        }

        Ok(bytes)
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        check_fixed_body(body, READ_REQUEST_FIXED_BYTES, READ_REQUEST_STRUCTURE_SIZE)?;

        let channel_info_offset = le_u16(body, 44) as usize;
        let channel_info_length = le_u16(body, 46) as usize;
        let channel_info = if channel_info_length == 0 {
            Vec::new()
        } else {
            checked_body_tail_slice(
                body,
                channel_info_offset,
                channel_info_length,
                READ_REQUEST_FIXED_BYTES,
            )?
            .to_vec()
        };

        Ok(Self {
            flags: Smb2ReadFlags(body[3]),
            length: le_u32(body, 4),
            offset: le_u64(body, 8),
            file_id: Smb2FileId {
                persistent: le_u64(body, 16),
                volatile: le_u64(body, 24),
            },
            minimum_count: le_u32(body, 32),
            channel: Smb2ReadChannel(le_u32(body, 36)),
            remaining_bytes: le_u32(body, 40),
            channel_info,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2ReadResponse {
    pub data_remaining: u32,
    pub data: Vec<u8>,
}

impl Smb2ReadResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body = response_body(
            packet,
            READ_RESPONSE_FIXED_BYTES,
            READ_RESPONSE_STRUCTURE_SIZE,
        )?;

        let data_offset = body[2] as usize;
        let data_length = le_u32(body, 4) as usize;
        let data = if data_length == 0 {
            Vec::new()
        } else {
            checked_packet_tail_slice(
                packet,
                data_offset,
                data_length,
                SMB2_HEADER_BYTES + READ_RESPONSE_FIXED_BYTES,
            )?
            .to_vec()
        };

        Ok(Self {
            data_remaining: le_u32(body, 8),
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn read_request_round_trips_range_read() {
        let request = Smb2ReadRequest {
            flags: Smb2ReadFlags::READ_UNBUFFERED,
            length: 1_048_576,
            offset: 8_388_608,
            file_id: Smb2FileId {
                persistent: 0x0102_0304_0506_0708,
                volatile: 0x1112_1314_1516_1718,
            },
            minimum_count: 4096,
            channel: Smb2ReadChannel::RDMA_V1,
            remaining_bytes: 0,
            channel_info: b"rdma".to_vec(),
        };

        let encoded = request.encode().expect("encode");
        let decoded = Smb2ReadRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), READ_REQUEST_STRUCTURE_SIZE);
        assert_eq!(
            le_u16(&encoded, 44),
            (SMB2_HEADER_BYTES + READ_REQUEST_FIXED_BYTES) as u16
        );
        assert_eq!(le_u16(&encoded, 46), 4);
    }

    #[test]
    fn read_request_encodes_without_channel_info() {
        let request = read_request(Vec::new());

        let encoded = request.encode().expect("encode");
        let decoded = Smb2ReadRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 44), 0);
        assert_eq!(le_u16(&encoded, 46), 0);
    }

    #[test]
    fn read_request_rejects_channel_info_before_body() {
        let request = read_request(b"info".to_vec());
        let mut encoded = request.encode().expect("encode");
        encoded[44..46].copy_from_slice(&(SMB2_HEADER_BYTES as u16 - 1).to_le_bytes());

        assert_eq!(
            Smb2ReadRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES - 1,
                length: 4,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn read_request_rejects_channel_info_inside_fixed_body() {
        let request = read_request(b"info".to_vec());
        let mut encoded = request.encode().expect("encode");
        encoded[44..46].copy_from_slice(&(SMB2_HEADER_BYTES as u16).to_le_bytes());

        assert_eq!(
            Smb2ReadRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES,
                length: 4,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn read_request_rejects_too_large_channel_info() {
        let request = read_request(vec![0; u16::MAX as usize + 1]);

        assert_eq!(
            request.encode(),
            Err(SmbClientError::FieldTooLarge {
                field: "read.channel_info",
                length: u16::MAX as usize + 1,
                max: u16::MAX as usize
            })
        );
    }

    #[test]
    fn read_response_decodes_data() {
        let mut packet = Smb2Header::new(Smb2Command::Read, 22).encode().to_vec();
        let data = b"payload";
        packet.extend_from_slice(&response_body(data, 9));
        packet.extend_from_slice(data);

        let response = Smb2ReadResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response.data_remaining, 9);
        assert_eq!(response.data, data);
    }

    #[test]
    fn read_response_rejects_data_outside_packet() {
        let mut packet = Smb2Header::new(Smb2Command::Read, 22).encode().to_vec();
        packet.extend_from_slice(&response_body(b"abc", 0));
        packet[SMB2_HEADER_BYTES + 4..SMB2_HEADER_BYTES + 8].copy_from_slice(&8u32.to_le_bytes());

        assert_eq!(
            Smb2ReadResponse::decode_packet(&packet),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + READ_RESPONSE_FIXED_BYTES,
                length: 8,
                available: packet.len()
            })
        );
    }

    #[test]
    fn read_response_rejects_data_inside_fixed_body() {
        let mut packet = Smb2Header::new(Smb2Command::Read, 22).encode().to_vec();
        packet.extend_from_slice(&response_body(b"abc", 0));
        packet[SMB2_HEADER_BYTES + 2] = SMB2_HEADER_BYTES as u8;

        assert_eq!(
            Smb2ReadResponse::decode_packet(&packet),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES,
                length: 3,
                available: packet.len()
            })
        );
    }

    fn read_request(channel_info: Vec<u8>) -> Smb2ReadRequest {
        Smb2ReadRequest {
            flags: Smb2ReadFlags::default(),
            length: 65536,
            offset: 0,
            file_id: Smb2FileId {
                persistent: 1,
                volatile: 2,
            },
            minimum_count: 0,
            channel: Smb2ReadChannel::NONE,
            remaining_bytes: 0,
            channel_info,
        }
    }

    fn response_body(data: &[u8], data_remaining: u32) -> Vec<u8> {
        let mut body = Vec::with_capacity(READ_RESPONSE_FIXED_BYTES);
        push_u16(&mut body, READ_RESPONSE_STRUCTURE_SIZE);
        body.push((SMB2_HEADER_BYTES + READ_RESPONSE_FIXED_BYTES) as u8);
        body.push(0);
        push_u32(&mut body, data.len() as u32);
        push_u32(&mut body, data_remaining);
        push_u32(&mut body, 0);
        body
    }
}
