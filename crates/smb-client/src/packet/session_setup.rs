use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{
    check_fixed_body, checked_slice, le_u16, le_u32, le_u64, push_u16, push_u32, push_u64,
    response_body,
};
use crate::SmbClientError;

const SESSION_SETUP_REQUEST_STRUCTURE_SIZE: u16 = 25;
const SESSION_SETUP_RESPONSE_STRUCTURE_SIZE: u16 = 9;
const SESSION_SETUP_REQUEST_FIXED_BYTES: usize = 24;
const SESSION_SETUP_RESPONSE_FIXED_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2SessionSetupSecurityMode(pub u8);

impl Smb2SessionSetupSecurityMode {
    pub const SIGNING_ENABLED: Self = Self(0x01);
    pub const SIGNING_REQUIRED: Self = Self(0x02);

    pub fn contains(self, mode: Self) -> bool {
        self.0 & mode.0 == mode.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2SessionSetupRequest {
    pub flags: u8,
    pub security_mode: Smb2SessionSetupSecurityMode,
    pub capabilities: u32,
    pub channel: u32,
    pub previous_session_id: u64,
    pub security_buffer: Vec<u8>,
}

impl Smb2SessionSetupRequest {
    pub fn encode(&self) -> Vec<u8> {
        let security_offset = (SMB2_HEADER_BYTES + SESSION_SETUP_REQUEST_FIXED_BYTES) as u16;
        let mut bytes =
            Vec::with_capacity(SESSION_SETUP_REQUEST_FIXED_BYTES + self.security_buffer.len());
        push_u16(&mut bytes, SESSION_SETUP_REQUEST_STRUCTURE_SIZE);
        bytes.push(self.flags);
        bytes.push(self.security_mode.0);
        push_u32(&mut bytes, self.capabilities);
        push_u32(&mut bytes, self.channel);
        push_u16(&mut bytes, security_offset);
        push_u16(&mut bytes, self.security_buffer.len() as u16);
        push_u64(&mut bytes, self.previous_session_id);
        bytes.extend_from_slice(&self.security_buffer);
        bytes
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        check_fixed_body(
            body,
            SESSION_SETUP_REQUEST_FIXED_BYTES,
            SESSION_SETUP_REQUEST_STRUCTURE_SIZE,
        )?;
        let security_offset = le_u16(body, 12) as usize;
        let security_length = le_u16(body, 14) as usize;
        let body_security_offset = security_offset.checked_sub(SMB2_HEADER_BYTES).ok_or(
            SmbClientError::BufferRangeOutOfBounds {
                offset: security_offset,
                length: security_length,
                available: SMB2_HEADER_BYTES + body.len(),
            },
        )?;
        let security_buffer = checked_slice(body, body_security_offset, security_length)?.to_vec();

        Ok(Self {
            flags: body[2],
            security_mode: Smb2SessionSetupSecurityMode(body[3]),
            capabilities: le_u32(body, 4),
            channel: le_u32(body, 8),
            previous_session_id: le_u64(body, 16),
            security_buffer,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2SessionSetupResponse {
    pub session_flags: u16,
    pub security_buffer: Vec<u8>,
}

impl Smb2SessionSetupResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body = response_body(
            packet,
            SESSION_SETUP_RESPONSE_FIXED_BYTES,
            SESSION_SETUP_RESPONSE_STRUCTURE_SIZE,
        )?;
        let security_offset = le_u16(body, 4) as usize;
        let security_length = le_u16(body, 6) as usize;
        let security_buffer = checked_slice(packet, security_offset, security_length)?.to_vec();

        Ok(Self {
            session_flags: le_u16(body, 2),
            security_buffer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn session_setup_request_round_trips_security_blob() {
        let request = Smb2SessionSetupRequest {
            flags: 0,
            security_mode: Smb2SessionSetupSecurityMode::SIGNING_ENABLED,
            capabilities: 0x0000_0040,
            channel: 0,
            previous_session_id: 0x0102_0304_0506_0708,
            security_buffer: b"ntlm-negotiate".to_vec(),
        };

        let encoded = request.encode();
        let decoded = Smb2SessionSetupRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), SESSION_SETUP_REQUEST_STRUCTURE_SIZE);
        assert_eq!(
            le_u16(&encoded, 12),
            (SMB2_HEADER_BYTES + SESSION_SETUP_REQUEST_FIXED_BYTES) as u16
        );
    }

    #[test]
    fn session_setup_request_rejects_bad_security_offset() {
        let request = Smb2SessionSetupRequest {
            flags: 0,
            security_mode: Smb2SessionSetupSecurityMode::SIGNING_ENABLED,
            capabilities: 0,
            channel: 0,
            previous_session_id: 0,
            security_buffer: b"blob".to_vec(),
        };
        let mut encoded = request.encode();
        encoded[12..14].copy_from_slice(&(SMB2_HEADER_BYTES as u16 - 1).to_le_bytes());

        assert_eq!(
            Smb2SessionSetupRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES - 1,
                length: 4,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn session_setup_response_decodes_security_blob() {
        let mut packet = Smb2Header::new(Smb2Command::SessionSetup, 4)
            .encode()
            .to_vec();
        let security = b"ntlm-challenge";
        packet.extend_from_slice(&response_body(security));
        packet.extend_from_slice(security);

        let response = Smb2SessionSetupResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response.session_flags, 0x0001);
        assert_eq!(response.security_buffer, security);
    }

    #[test]
    fn session_setup_response_rejects_security_blob_outside_packet() {
        let mut packet = Smb2Header::new(Smb2Command::SessionSetup, 4)
            .encode()
            .to_vec();
        packet.extend_from_slice(&response_body(b"token"));

        assert_eq!(
            Smb2SessionSetupResponse::decode_packet(&packet),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + SESSION_SETUP_RESPONSE_FIXED_BYTES,
                length: 5,
                available: SMB2_HEADER_BYTES + SESSION_SETUP_RESPONSE_FIXED_BYTES
            })
        );
    }

    fn response_body(security: &[u8]) -> [u8; SESSION_SETUP_RESPONSE_FIXED_BYTES] {
        let mut bytes = [0; SESSION_SETUP_RESPONSE_FIXED_BYTES];
        bytes[0..2].copy_from_slice(&SESSION_SETUP_RESPONSE_STRUCTURE_SIZE.to_le_bytes());
        bytes[2..4].copy_from_slice(&0x0001u16.to_le_bytes());
        bytes[4..6].copy_from_slice(
            &((SMB2_HEADER_BYTES + SESSION_SETUP_RESPONSE_FIXED_BYTES) as u16).to_le_bytes(),
        );
        bytes[6..8].copy_from_slice(&(security.len() as u16).to_le_bytes());
        bytes
    }
}
