use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{checked_slice, le_u16, le_u32, le_u64};
use crate::SmbClientError;

const NEGOTIATE_REQUEST_STRUCTURE_SIZE: u16 = 36;
const NEGOTIATE_RESPONSE_STRUCTURE_SIZE: u16 = 65;
const NEGOTIATE_REQUEST_FIXED_BYTES: usize = 36;
const NEGOTIATE_RESPONSE_FIXED_BYTES: usize = 65;
const NEGOTIATE_CONTEXT_HEADER_BYTES: usize = 8;
const PREAUTH_INTEGRITY_CAPABILITIES: u16 = 0x0001;
const ENCRYPTION_CAPABILITIES: u16 = 0x0002;
const SHA512_ALGORITHM: u16 = 0x0001;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum Smb2Dialect {
    Smb202 = 0x0202,
    Smb210 = 0x0210,
    Smb300 = 0x0300,
    Smb302 = 0x0302,
    Smb311 = 0x0311,
}

impl Smb2Dialect {
    pub fn from_wire(value: u16) -> Result<Self, SmbClientError> {
        match value {
            0x0202 => Ok(Self::Smb202),
            0x0210 => Ok(Self::Smb210),
            0x0300 => Ok(Self::Smb300),
            0x0302 => Ok(Self::Smb302),
            0x0311 => Ok(Self::Smb311),
            dialect => Err(SmbClientError::UnknownDialect { dialect }),
        }
    }

    pub fn to_wire(self) -> u16 {
        self as u16
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum Smb2EncryptionCipher {
    Aes128Ccm = 0x0001,
    Aes128Gcm = 0x0002,
    Aes256Ccm = 0x0003,
    Aes256Gcm = 0x0004,
}

impl Smb2EncryptionCipher {
    fn from_wire(value: u16) -> Option<Self> {
        match value {
            0x0001 => Some(Self::Aes128Ccm),
            0x0002 => Some(Self::Aes128Gcm),
            0x0003 => Some(Self::Aes256Ccm),
            0x0004 => Some(Self::Aes256Gcm),
            _ => None,
        }
    }

    fn to_wire(self) -> u16 {
        self as u16
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Smb2NegotiateContext {
    PreauthIntegritySha512 { salt: Vec<u8> },
    EncryptionCapabilities { ciphers: Vec<Smb2EncryptionCipher> },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2SecurityMode(pub u16);

impl Smb2SecurityMode {
    pub const SIGNING_ENABLED: Self = Self(0x0001);
    pub const SIGNING_REQUIRED: Self = Self(0x0002);

    pub fn contains(self, mode: Self) -> bool {
        self.0 & mode.0 == mode.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2NegotiateRequest {
    pub security_mode: Smb2SecurityMode,
    pub capabilities: u32,
    pub client_guid: [u8; 16],
    pub dialects: Vec<Smb2Dialect>,
    pub contexts: Vec<Smb2NegotiateContext>,
}

impl Smb2NegotiateRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(NEGOTIATE_REQUEST_FIXED_BYTES + self.dialects.len() * 2);
        push_u16(&mut bytes, NEGOTIATE_REQUEST_STRUCTURE_SIZE);
        push_u16(&mut bytes, self.dialects.len() as u16);
        push_u16(&mut bytes, self.security_mode.0);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, self.capabilities);
        bytes.extend_from_slice(&self.client_guid);
        let context_offset_field = bytes.len();
        push_u32(&mut bytes, 0);
        push_u16(&mut bytes, self.contexts.len() as u16);
        push_u16(&mut bytes, 0);
        for dialect in &self.dialects {
            push_u16(&mut bytes, dialect.to_wire());
        }
        if !self.contexts.is_empty() {
            pad_to_eight(&mut bytes, SMB2_HEADER_BYTES);
            let context_offset = SMB2_HEADER_BYTES + bytes.len();
            bytes[context_offset_field..context_offset_field + 4]
                .copy_from_slice(&(context_offset as u32).to_le_bytes());
            encode_contexts(&mut bytes, &self.contexts);
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SmbClientError> {
        if bytes.len() < NEGOTIATE_REQUEST_FIXED_BYTES {
            return Err(SmbClientError::ShortFrame {
                expected: NEGOTIATE_REQUEST_FIXED_BYTES,
                actual: bytes.len(),
            });
        }

        let structure_size = le_u16(bytes, 0);
        if structure_size != NEGOTIATE_REQUEST_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        let dialect_count = le_u16(bytes, 2) as usize;
        let dialect_bytes = dialect_count.saturating_mul(2);
        let required_len = NEGOTIATE_REQUEST_FIXED_BYTES.saturating_add(dialect_bytes);
        if bytes.len() < required_len {
            return Err(SmbClientError::ShortFrame {
                expected: required_len,
                actual: bytes.len(),
            });
        }

        let mut client_guid = [0; 16];
        client_guid.copy_from_slice(&bytes[12..28]);
        let mut dialects = Vec::with_capacity(dialect_count);
        for index in 0..dialect_count {
            let offset = NEGOTIATE_REQUEST_FIXED_BYTES + index * 2;
            dialects.push(Smb2Dialect::from_wire(le_u16(bytes, offset))?);
        }

        let context_count = le_u16(bytes, 32) as usize;
        let contexts = if context_count == 0 {
            Vec::new()
        } else {
            let absolute_offset = le_u32(bytes, 28) as usize;
            let context_offset = absolute_offset.checked_sub(SMB2_HEADER_BYTES).ok_or(
                SmbClientError::BufferRangeOutOfBounds {
                    offset: absolute_offset,
                    length: NEGOTIATE_CONTEXT_HEADER_BYTES,
                    available: SMB2_HEADER_BYTES + bytes.len(),
                },
            )?;
            if !absolute_offset.is_multiple_of(8) || context_offset < required_len {
                return Err(SmbClientError::InvalidNegotiateContext {
                    reason: "request context offset is unaligned or overlaps dialects",
                });
            }
            decode_contexts(bytes, context_offset, context_count)?
        };

        Ok(Self {
            security_mode: Smb2SecurityMode(le_u16(bytes, 4)),
            capabilities: le_u32(bytes, 8),
            client_guid,
            dialects,
            contexts,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2NegotiateResponse {
    pub security_mode: Smb2SecurityMode,
    pub dialect: Smb2Dialect,
    pub negotiate_context_count: u16,
    pub server_guid: [u8; 16],
    pub capabilities: u32,
    pub max_transact_size: u32,
    pub max_read_size: u32,
    pub max_write_size: u32,
    pub system_time: u64,
    pub server_start_time: u64,
    pub security_buffer: Vec<u8>,
    pub negotiate_context_offset: u32,
    pub encryption_cipher: Option<Smb2EncryptionCipher>,
}

impl Smb2NegotiateResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body_offset = SMB2_HEADER_BYTES;
        let required_len = body_offset + NEGOTIATE_RESPONSE_FIXED_BYTES;
        if packet.len() < required_len {
            return Err(SmbClientError::ShortFrame {
                expected: required_len,
                actual: packet.len(),
            });
        }
        let body = &packet[body_offset..];
        let structure_size = le_u16(body, 0);
        if structure_size != NEGOTIATE_RESPONSE_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        let security_buffer_offset = le_u16(body, 56) as usize;
        let security_buffer_length = le_u16(body, 58) as usize;
        let security_buffer =
            checked_slice(packet, security_buffer_offset, security_buffer_length)?.to_vec();

        let mut server_guid = [0; 16];
        server_guid.copy_from_slice(&body[8..24]);

        let negotiate_context_count = le_u16(body, 6);
        let negotiate_context_offset = le_u32(body, 60);
        let contexts = if negotiate_context_count == 0 {
            Vec::new()
        } else {
            if !(negotiate_context_offset as usize).is_multiple_of(8)
                || (negotiate_context_offset as usize)
                    < SMB2_HEADER_BYTES + NEGOTIATE_RESPONSE_FIXED_BYTES
            {
                return Err(SmbClientError::InvalidNegotiateContext {
                    reason: "response context offset is unaligned or overlaps fixed fields",
                });
            }
            decode_contexts(
                packet,
                negotiate_context_offset as usize,
                negotiate_context_count as usize,
            )?
        };
        let encryption_cipher = contexts.iter().find_map(|context| match context {
            Smb2NegotiateContext::EncryptionCapabilities { ciphers } => ciphers.first().copied(),
            Smb2NegotiateContext::PreauthIntegritySha512 { .. } => None,
        });

        Ok(Self {
            security_mode: Smb2SecurityMode(le_u16(body, 2)),
            dialect: Smb2Dialect::from_wire(le_u16(body, 4))?,
            negotiate_context_count,
            server_guid,
            capabilities: le_u32(body, 24),
            max_transact_size: le_u32(body, 28),
            max_read_size: le_u32(body, 32),
            max_write_size: le_u32(body, 36),
            system_time: le_u64(body, 40),
            server_start_time: le_u64(body, 48),
            security_buffer,
            negotiate_context_offset,
            encryption_cipher,
        })
    }
}

fn encode_contexts(bytes: &mut Vec<u8>, contexts: &[Smb2NegotiateContext]) {
    for (index, context) in contexts.iter().enumerate() {
        let (context_type, data) = match context {
            Smb2NegotiateContext::PreauthIntegritySha512 { salt } => {
                let mut data = Vec::with_capacity(6 + salt.len());
                push_u16(&mut data, 1);
                push_u16(&mut data, salt.len() as u16);
                push_u16(&mut data, SHA512_ALGORITHM);
                data.extend_from_slice(salt);
                (PREAUTH_INTEGRITY_CAPABILITIES, data)
            }
            Smb2NegotiateContext::EncryptionCapabilities { ciphers } => {
                let mut data = Vec::with_capacity(2 + ciphers.len() * 2);
                push_u16(&mut data, ciphers.len() as u16);
                for cipher in ciphers {
                    push_u16(&mut data, cipher.to_wire());
                }
                (ENCRYPTION_CAPABILITIES, data)
            }
        };
        push_u16(bytes, context_type);
        push_u16(bytes, data.len() as u16);
        push_u32(bytes, 0);
        bytes.extend_from_slice(&data);
        if index + 1 < contexts.len() {
            pad_to_eight(bytes, 0);
        }
    }
}

fn decode_contexts(
    bytes: &[u8],
    mut offset: usize,
    count: usize,
) -> Result<Vec<Smb2NegotiateContext>, SmbClientError> {
    let mut contexts = Vec::with_capacity(count);
    let mut seen_preauth = false;
    let mut seen_encryption = false;
    for index in 0..count {
        let header = checked_slice(bytes, offset, NEGOTIATE_CONTEXT_HEADER_BYTES)?;
        if header[4..8] != [0; 4] {
            return Err(SmbClientError::InvalidNegotiateContext {
                reason: "context reserved field is not zero",
            });
        }
        let context_type = le_u16(header, 0);
        let data_length = le_u16(header, 2) as usize;
        let data_offset = offset + NEGOTIATE_CONTEXT_HEADER_BYTES;
        let data = checked_slice(bytes, data_offset, data_length)?;
        match context_type {
            PREAUTH_INTEGRITY_CAPABILITIES => {
                if seen_preauth {
                    return Err(SmbClientError::InvalidNegotiateContext {
                        reason: "duplicate preauth integrity context",
                    });
                }
                seen_preauth = true;
                if data.len() < 6 || le_u16(data, 0) != 1 || le_u16(data, 4) != SHA512_ALGORITHM {
                    return Err(SmbClientError::InvalidNegotiateContext {
                        reason: "preauth integrity must select SHA-512",
                    });
                }
                let salt_length = le_u16(data, 2) as usize;
                if data.len() != 6 + salt_length {
                    return Err(SmbClientError::InvalidNegotiateContext {
                        reason: "preauth integrity salt length mismatch",
                    });
                }
                contexts.push(Smb2NegotiateContext::PreauthIntegritySha512 {
                    salt: data[6..].to_vec(),
                });
            }
            ENCRYPTION_CAPABILITIES => {
                if seen_encryption {
                    return Err(SmbClientError::InvalidNegotiateContext {
                        reason: "duplicate encryption capabilities context",
                    });
                }
                seen_encryption = true;
                if data.len() < 2 {
                    return Err(SmbClientError::InvalidNegotiateContext {
                        reason: "missing encryption cipher count",
                    });
                }
                let cipher_count = le_u16(data, 0) as usize;
                if cipher_count == 0 || data.len() != 2 + cipher_count * 2 {
                    return Err(SmbClientError::InvalidNegotiateContext {
                        reason: "encryption cipher count mismatch",
                    });
                }
                let mut ciphers = Vec::with_capacity(cipher_count);
                for cipher_index in 0..cipher_count {
                    if let Some(cipher) =
                        Smb2EncryptionCipher::from_wire(le_u16(data, 2 + cipher_index * 2))
                    {
                        ciphers.push(cipher);
                    }
                }
                contexts.push(Smb2NegotiateContext::EncryptionCapabilities { ciphers });
            }
            _ => {}
        }
        offset = data_offset + data_length;
        if index + 1 < count {
            offset = align_eight(offset);
        }
    }
    Ok(contexts)
}

fn pad_to_eight(bytes: &mut Vec<u8>, base_offset: usize) {
    let padding = align_eight(base_offset + bytes.len()) - (base_offset + bytes.len());
    bytes.resize(bytes.len() + padding, 0);
}

fn align_eight(value: usize) -> usize {
    value.saturating_add(7) & !7
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn negotiate_request_round_trips_dialects() {
        let request = Smb2NegotiateRequest {
            security_mode: Smb2SecurityMode::SIGNING_ENABLED,
            capabilities: 0x0000_0040,
            client_guid: [0x22; 16],
            dialects: vec![
                Smb2Dialect::Smb202,
                Smb2Dialect::Smb210,
                Smb2Dialect::Smb300,
                Smb2Dialect::Smb302,
                Smb2Dialect::Smb311,
            ],
            contexts: Vec::new(),
        };

        let encoded = request.encode();
        let decoded = Smb2NegotiateRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), NEGOTIATE_REQUEST_STRUCTURE_SIZE);
        assert_eq!(le_u16(&encoded, 2), 5);
    }

    #[test]
    fn negotiate_request_rejects_short_dialect_array() {
        let request = Smb2NegotiateRequest {
            security_mode: Smb2SecurityMode::SIGNING_ENABLED,
            capabilities: 0,
            client_guid: [0; 16],
            dialects: vec![Smb2Dialect::Smb311],
            contexts: Vec::new(),
        };
        let mut encoded = request.encode();
        encoded.pop();

        assert_eq!(
            Smb2NegotiateRequest::decode(&encoded),
            Err(SmbClientError::ShortFrame {
                expected: NEGOTIATE_REQUEST_FIXED_BYTES + 2,
                actual: NEGOTIATE_REQUEST_FIXED_BYTES + 1
            })
        );
    }

    #[test]
    fn negotiate_request_round_trips_smb311_security_contexts() {
        let request = Smb2NegotiateRequest {
            security_mode: Smb2SecurityMode::SIGNING_ENABLED,
            capabilities: 0x0000_0040,
            client_guid: [0x22; 16],
            dialects: vec![Smb2Dialect::Smb311],
            contexts: vec![
                Smb2NegotiateContext::PreauthIntegritySha512 {
                    salt: vec![0x33; 32],
                },
                Smb2NegotiateContext::EncryptionCapabilities {
                    ciphers: vec![Smb2EncryptionCipher::Aes128Gcm],
                },
            ],
        };

        let encoded = request.encode();
        let context_offset = le_u32(&encoded, 28) as usize;

        assert!(context_offset.is_multiple_of(8));
        assert_eq!(le_u16(&encoded, 32), 2);
        assert_eq!(Smb2NegotiateRequest::decode(&encoded), Ok(request));
    }

    #[test]
    fn negotiate_response_decodes_security_buffer() {
        let mut packet = Smb2Header::new(Smb2Command::Negotiate, 7).encode().to_vec();
        let security = b"ntlm-token";
        packet.extend_from_slice(&response_body(security, Smb2Dialect::Smb311));
        packet.extend_from_slice(security);

        let response = Smb2NegotiateResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response.security_mode, Smb2SecurityMode::SIGNING_ENABLED);
        assert_eq!(response.dialect, Smb2Dialect::Smb311);
        assert_eq!(response.security_buffer, security);
        assert_eq!(response.max_read_size, 1024 * 1024);
        assert_eq!(response.max_write_size, 1024 * 1024);
    }

    #[test]
    fn negotiate_response_rejects_security_buffer_outside_packet() {
        let mut packet = Smb2Header::new(Smb2Command::Negotiate, 7).encode().to_vec();
        packet.extend_from_slice(&response_body(b"token", Smb2Dialect::Smb311));
        let error = Smb2NegotiateResponse::decode_packet(&packet).expect_err("reject");

        assert_eq!(
            error,
            SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + NEGOTIATE_RESPONSE_FIXED_BYTES,
                length: 5,
                available: SMB2_HEADER_BYTES + NEGOTIATE_RESPONSE_FIXED_BYTES
            }
        );
    }

    #[test]
    fn negotiate_response_selects_aes128_gcm() {
        let mut packet = Smb2Header::new(Smb2Command::Negotiate, 7).encode().to_vec();
        let mut body = response_body(b"", Smb2Dialect::Smb311);
        put_u16(&mut body, 6, 2);
        let context_offset = align_eight(SMB2_HEADER_BYTES + body.len());
        put_u32(&mut body, 60, context_offset as u32);
        packet.extend_from_slice(&body);
        packet.resize(context_offset, 0);
        encode_contexts(
            &mut packet,
            &[
                Smb2NegotiateContext::PreauthIntegritySha512 { salt: Vec::new() },
                Smb2NegotiateContext::EncryptionCapabilities {
                    ciphers: vec![Smb2EncryptionCipher::Aes128Gcm],
                },
            ],
        );

        let response = Smb2NegotiateResponse::decode_packet(&packet).expect("decode");

        assert_eq!(
            response.encryption_cipher,
            Some(Smb2EncryptionCipher::Aes128Gcm)
        );
    }

    fn response_body(
        security: &[u8],
        dialect: Smb2Dialect,
    ) -> [u8; NEGOTIATE_RESPONSE_FIXED_BYTES] {
        let mut bytes = [0; NEGOTIATE_RESPONSE_FIXED_BYTES];
        put_u16(&mut bytes, 0, NEGOTIATE_RESPONSE_STRUCTURE_SIZE);
        put_u16(&mut bytes, 2, Smb2SecurityMode::SIGNING_ENABLED.0);
        put_u16(&mut bytes, 4, dialect.to_wire());
        put_u16(&mut bytes, 6, 0);
        bytes[8..24].copy_from_slice(&[0x33; 16]);
        put_u32(&mut bytes, 24, 0x0000_0040);
        put_u32(&mut bytes, 28, 1024 * 1024);
        put_u32(&mut bytes, 32, 1024 * 1024);
        put_u32(&mut bytes, 36, 1024 * 1024);
        put_u64(&mut bytes, 40, 1);
        put_u64(&mut bytes, 48, 2);
        put_u16(
            &mut bytes,
            56,
            (SMB2_HEADER_BYTES + NEGOTIATE_RESPONSE_FIXED_BYTES) as u16,
        );
        put_u16(&mut bytes, 58, security.len() as u16);
        put_u32(&mut bytes, 60, 0);
        bytes
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
