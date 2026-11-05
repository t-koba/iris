use crate::packet::create::Smb2FileId;
use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{
    check_fixed_body, checked_body_tail_slice, checked_packet_tail_slice, checked_u16_len,
    checked_u32_len, le_u16, le_u32, le_u64, push_u16, push_u32, push_u64, response_body,
};
use crate::SmbClientError;

const WRITE_REQUEST_STRUCTURE_SIZE: u16 = 49;
const WRITE_RESPONSE_STRUCTURE_SIZE: u16 = 17;
const WRITE_REQUEST_FIXED_BYTES: usize = 48;
const WRITE_RESPONSE_FIXED_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2WriteChannel(pub u32);

impl Smb2WriteChannel {
    pub const NONE: Self = Self(0x0000_0000);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2WriteFlags(pub u32);

impl Smb2WriteFlags {
    pub const WRITE_THROUGH: Self = Self(0x0000_0001);

    pub fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2WriteRequest {
    pub offset: u64,
    pub file_id: Smb2FileId,
    pub channel: Smb2WriteChannel,
    pub remaining_bytes: u32,
    pub flags: Smb2WriteFlags,
    pub data: Vec<u8>,
    pub channel_info: Vec<u8>,
}

impl Smb2WriteRequest {
    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let data_length = checked_u32_len("write.data", self.data.len())?;
        let channel_info_length = checked_u16_len("write.channel_info", self.channel_info.len())?;
        let mut bytes = Vec::with_capacity(
            WRITE_REQUEST_FIXED_BYTES + self.data.len() + self.channel_info.len(),
        );
        push_u16(&mut bytes, WRITE_REQUEST_STRUCTURE_SIZE);
        if self.data.is_empty() {
            push_u16(&mut bytes, 0);
        } else {
            push_u16(
                &mut bytes,
                (SMB2_HEADER_BYTES + WRITE_REQUEST_FIXED_BYTES) as u16,
            );
        }
        push_u32(&mut bytes, data_length);
        push_u64(&mut bytes, self.offset);
        push_u64(&mut bytes, self.file_id.persistent);
        push_u64(&mut bytes, self.file_id.volatile);
        push_u32(&mut bytes, self.channel.0);
        push_u32(&mut bytes, self.remaining_bytes);

        if self.channel_info.is_empty() {
            push_u16(&mut bytes, 0);
            push_u16(&mut bytes, 0);
        } else {
            let channel_info_offset =
                SMB2_HEADER_BYTES + WRITE_REQUEST_FIXED_BYTES + self.data.len();
            push_u16(
                &mut bytes,
                checked_u16_len("write.channel_info_offset", channel_info_offset)?,
            );
            push_u16(&mut bytes, channel_info_length);
        }
        push_u32(&mut bytes, self.flags.0);
        bytes.extend_from_slice(&self.data);
        bytes.extend_from_slice(&self.channel_info);
        Ok(bytes)
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        check_fixed_body(
            body,
            WRITE_REQUEST_FIXED_BYTES,
            WRITE_REQUEST_STRUCTURE_SIZE,
        )?;

        let data_offset = le_u16(body, 2) as usize;
        let data_length = le_u32(body, 4) as usize;
        let data = if data_length == 0 {
            Vec::new()
        } else {
            checked_body_tail_slice(body, data_offset, data_length, WRITE_REQUEST_FIXED_BYTES)?
                .to_vec()
        };

        let channel_info_offset = le_u16(body, 40) as usize;
        let channel_info_length = le_u16(body, 42) as usize;
        let channel_info = if channel_info_length == 0 {
            Vec::new()
        } else {
            checked_body_tail_slice(
                body,
                channel_info_offset,
                channel_info_length,
                WRITE_REQUEST_FIXED_BYTES,
            )?
            .to_vec()
        };

        Ok(Self {
            offset: le_u64(body, 8),
            file_id: Smb2FileId {
                persistent: le_u64(body, 16),
                volatile: le_u64(body, 24),
            },
            channel: Smb2WriteChannel(le_u32(body, 32)),
            remaining_bytes: le_u32(body, 36),
            flags: Smb2WriteFlags(le_u32(body, 44)),
            data,
            channel_info,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2WriteResponse {
    pub count: u32,
    pub remaining: u32,
    pub channel_info: Vec<u8>,
}

impl Smb2WriteResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body = response_body(
            packet,
            WRITE_RESPONSE_FIXED_BYTES,
            WRITE_RESPONSE_STRUCTURE_SIZE,
        )?;

        let channel_info_offset = le_u16(body, 12) as usize;
        let channel_info_length = le_u16(body, 14) as usize;
        let channel_info = if channel_info_length == 0 {
            Vec::new()
        } else {
            checked_packet_tail_slice(
                packet,
                channel_info_offset,
                channel_info_length,
                SMB2_HEADER_BYTES + WRITE_RESPONSE_FIXED_BYTES,
            )?
            .to_vec()
        };

        Ok(Self {
            count: le_u32(body, 4),
            remaining: le_u32(body, 8),
            channel_info,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn write_request_round_trips_data_and_channel_info() {
        let file_id = Smb2FileId {
            persistent: 0x0102_0304_0506_0708,
            volatile: 0x1112_1314_1516_1718,
        };
        let request = Smb2WriteRequest {
            offset: 4096,
            file_id,
            channel: Smb2WriteChannel::NONE,
            remaining_bytes: 7,
            flags: Smb2WriteFlags::WRITE_THROUGH,
            data: b"payload".to_vec(),
            channel_info: b"channel".to_vec(),
        };

        let encoded = request.encode().expect("encode");
        let decoded = Smb2WriteRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), WRITE_REQUEST_STRUCTURE_SIZE);
        assert_eq!(
            le_u16(&encoded, 2),
            (SMB2_HEADER_BYTES + WRITE_REQUEST_FIXED_BYTES) as u16
        );
        assert!(decoded.flags.contains(Smb2WriteFlags::WRITE_THROUGH));
    }

    #[test]
    fn write_request_rejects_data_inside_fixed_body() {
        let mut encoded = write_request_with_data(b"payload")
            .encode()
            .expect("encode");
        encoded[2..4].copy_from_slice(&(SMB2_HEADER_BYTES as u16 + 1).to_le_bytes());

        assert_eq!(
            Smb2WriteRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + 1,
                length: 7,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn write_response_decodes_count_and_channel_info() {
        let mut packet = Smb2Header::new(Smb2Command::Write, 9).encode().to_vec();
        packet.extend_from_slice(&response_body(7, b"channel"));
        packet.extend_from_slice(b"channel");

        let response = Smb2WriteResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response.count, 7);
        assert_eq!(response.remaining, 0);
        assert_eq!(response.channel_info, b"channel");
    }

    #[test]
    fn write_response_rejects_channel_info_outside_packet() {
        let mut packet = Smb2Header::new(Smb2Command::Write, 9).encode().to_vec();
        packet.extend_from_slice(&response_body(7, b"channel"));

        assert_eq!(
            Smb2WriteResponse::decode_packet(&packet),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + WRITE_RESPONSE_FIXED_BYTES,
                length: 7,
                available: SMB2_HEADER_BYTES + WRITE_RESPONSE_FIXED_BYTES
            })
        );
    }

    fn write_request_with_data(data: &[u8]) -> Smb2WriteRequest {
        Smb2WriteRequest {
            offset: 0,
            file_id: Smb2FileId {
                persistent: 1,
                volatile: 2,
            },
            channel: Smb2WriteChannel::NONE,
            remaining_bytes: 0,
            flags: Smb2WriteFlags::default(),
            data: data.to_vec(),
            channel_info: Vec::new(),
        }
    }

    fn response_body(count: u32, channel_info: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(WRITE_RESPONSE_FIXED_BYTES);
        push_u16(&mut body, WRITE_RESPONSE_STRUCTURE_SIZE);
        push_u16(&mut body, 0);
        push_u32(&mut body, count);
        push_u32(&mut body, 0);
        if channel_info.is_empty() {
            push_u16(&mut body, 0);
            push_u16(&mut body, 0);
        } else {
            push_u16(
                &mut body,
                (SMB2_HEADER_BYTES + WRITE_RESPONSE_FIXED_BYTES) as u16,
            );
            push_u16(&mut body, channel_info.len() as u16);
        }
        body
    }
}
