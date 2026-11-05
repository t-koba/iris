use crate::auth::ntlm::{ntlm_v2_hash, ntlm_v2_response, NtlmV2Response};
use crate::wire::{
    checked_slice, checked_u16_len, checked_u32_len, encode_utf16le, le_u16, le_u32, push_u16,
    push_u32,
};
use crate::SmbClientError;
use hmac::{Hmac, KeyInit, Mac};
use md5::Md5;
use std::ops::BitOr;

const NTLMSSP_SIGNATURE: &[u8; 8] = b"NTLMSSP\0";
const NEGOTIATE_MESSAGE_TYPE: u32 = 1;
const CHALLENGE_MESSAGE_TYPE: u32 = 2;
const AUTHENTICATE_MESSAGE_TYPE: u32 = 3;
const NEGOTIATE_FIXED_BYTES: usize = 32;
const CHALLENGE_FIXED_BYTES: usize = 48;
const AUTHENTICATE_FIXED_BYTES: usize = 88;
const AUTHENTICATE_MIC_OFFSET: usize = 72;
const MSV_AV_EOL: u16 = 0;
const MSV_AV_FLAGS: u16 = 6;
const MSV_AV_TIMESTAMP: u16 = 7;
const MSV_AV_TARGET_NAME: u16 = 9;
const MSV_AV_CHANNEL_BINDINGS: u16 = 10;
const MSV_AV_FLAG_MIC_PRESENT: u32 = 0x0000_0002;
const NTLM_VERSION: [u8; 8] = [10, 0, 0, 0, 0, 0, 0, 15];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NtlmsspNegotiateFlags(pub u32);

impl NtlmsspNegotiateFlags {
    pub const UNICODE: Self = Self(0x0000_0001);
    pub const REQUEST_TARGET: Self = Self(0x0000_0004);
    pub const SIGN: Self = Self(0x0000_0010);
    pub const SEAL: Self = Self(0x0000_0020);
    pub const NTLM: Self = Self(0x0000_0200);
    pub const ALWAYS_SIGN: Self = Self(0x0000_8000);
    pub const EXTENDED_SESSION_SECURITY: Self = Self(0x0008_0000);
    pub const TARGET_INFO: Self = Self(0x0080_0000);
    pub const VERSION: Self = Self(0x0200_0000);
    pub const KEY_128: Self = Self(0x2000_0000);
    pub const KEY_EXCH: Self = Self(0x4000_0000);
    pub const KEY_56: Self = Self(0x8000_0000);

    pub fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

impl BitOr for NtlmsspNegotiateFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtlmsspNegotiate {
    pub flags: NtlmsspNegotiateFlags,
    pub domain: String,
    pub workstation: String,
}

impl NtlmsspNegotiate {
    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let domain = encode_utf16le(&self.domain);
        let workstation = encode_utf16le(&self.workstation);
        let mut payload_offset = NEGOTIATE_FIXED_BYTES;
        let domain_offset = payload_offset;
        payload_offset = checked_add(payload_offset, domain.len())?;
        let workstation_offset = payload_offset;
        payload_offset = checked_add(payload_offset, workstation.len())?;

        let mut bytes = Vec::with_capacity(payload_offset);
        bytes.extend_from_slice(NTLMSSP_SIGNATURE);
        push_u32(&mut bytes, NEGOTIATE_MESSAGE_TYPE);
        push_u32(&mut bytes, self.flags.0);
        push_security_buffer(&mut bytes, domain.len(), domain_offset)?;
        push_security_buffer(&mut bytes, workstation.len(), workstation_offset)?;
        bytes.extend_from_slice(&domain);
        bytes.extend_from_slice(&workstation);
        Ok(bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtlmsspChallenge {
    pub target_name: Vec<u8>,
    pub flags: NtlmsspNegotiateFlags,
    pub server_challenge: [u8; 8],
    pub target_info: Vec<u8>,
    pub raw_message: Vec<u8>,
}

impl NtlmsspChallenge {
    pub fn decode(bytes: &[u8]) -> Result<Self, SmbClientError> {
        validate_signature_and_type(bytes, CHALLENGE_MESSAGE_TYPE)?;
        if bytes.len() < CHALLENGE_FIXED_BYTES {
            return Err(SmbClientError::ShortFrame {
                expected: CHALLENGE_FIXED_BYTES,
                actual: bytes.len(),
            });
        }

        let target_name = security_buffer_slice(bytes, 12)?.to_vec();
        let mut server_challenge = [0; 8];
        server_challenge.copy_from_slice(&bytes[24..32]);
        let target_info = security_buffer_slice(bytes, 40)?.to_vec();

        Ok(Self {
            target_name,
            flags: NtlmsspNegotiateFlags(le_u32(bytes, 20)),
            server_challenge,
            target_info,
            raw_message: bytes.to_vec(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtlmsspAuthenticate {
    pub flags: NtlmsspNegotiateFlags,
    pub domain: String,
    pub user: String,
    pub workstation: String,
    pub ntlm_v2: NtlmV2Response,
    pub encrypted_random_session_key: Vec<u8>,
    pub version: [u8; 8],
    pub mic: [u8; 16],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtlmsspAuthenticateResult {
    pub message: Vec<u8>,
    pub exported_session_key: [u8; 16],
}

pub struct NtlmV2AuthenticateRequest<'a> {
    pub user: &'a str,
    pub domain: &'a str,
    pub password: &'a str,
    pub workstation: &'a str,
    pub challenge: &'a NtlmsspChallenge,
    pub timestamp: u64,
    pub client_challenge: [u8; 8],
    pub negotiate_message: &'a [u8],
    pub target_name: &'a str,
}

impl NtlmsspAuthenticate {
    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let domain = encode_utf16le(&self.domain);
        let user = encode_utf16le(&self.user);
        let workstation = encode_utf16le(&self.workstation);
        let lm = &self.ntlm_v2.lm_challenge_response;
        let nt = &self.ntlm_v2.nt_challenge_response;
        let session_key = &self.encrypted_random_session_key;

        let mut fields = Vec::with_capacity(6);
        let mut payload_offset = AUTHENTICATE_FIXED_BYTES;
        for payload in [
            lm.as_slice(),
            nt.as_slice(),
            &domain,
            &user,
            &workstation,
            session_key,
        ] {
            let offset = payload_offset;
            payload_offset = checked_add(payload_offset, payload.len())?;
            fields.push(SecurityBufferField {
                length: payload.len(),
                offset,
            });
        }

        let mut bytes = Vec::with_capacity(payload_offset);
        bytes.extend_from_slice(NTLMSSP_SIGNATURE);
        push_u32(&mut bytes, AUTHENTICATE_MESSAGE_TYPE);
        for field in &fields {
            push_security_buffer(&mut bytes, field.length, field.offset)?;
        }
        push_u32(&mut bytes, self.flags.0);
        bytes.extend_from_slice(&self.version);
        debug_assert_eq!(bytes.len(), AUTHENTICATE_MIC_OFFSET);
        bytes.extend_from_slice(&self.mic);
        bytes.extend_from_slice(lm);
        bytes.extend_from_slice(nt);
        bytes.extend_from_slice(&domain);
        bytes.extend_from_slice(&user);
        bytes.extend_from_slice(&workstation);
        bytes.extend_from_slice(session_key);
        Ok(bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SecurityBufferField {
    length: usize,
    offset: usize,
}

pub fn default_negotiate_flags() -> NtlmsspNegotiateFlags {
    NtlmsspNegotiateFlags::UNICODE
        | NtlmsspNegotiateFlags::REQUEST_TARGET
        | NtlmsspNegotiateFlags::SIGN
        | NtlmsspNegotiateFlags::SEAL
        | NtlmsspNegotiateFlags::NTLM
        | NtlmsspNegotiateFlags::ALWAYS_SIGN
        | NtlmsspNegotiateFlags::EXTENDED_SESSION_SECURITY
        | NtlmsspNegotiateFlags::TARGET_INFO
        | NtlmsspNegotiateFlags::VERSION
        | NtlmsspNegotiateFlags::KEY_128
        | NtlmsspNegotiateFlags::KEY_EXCH
        | NtlmsspNegotiateFlags::KEY_56
}

pub fn ntlm_v2_authenticate_message(
    request: NtlmV2AuthenticateRequest<'_>,
) -> Result<NtlmsspAuthenticateResult, SmbClientError> {
    let NtlmV2AuthenticateRequest {
        user,
        domain,
        password,
        workstation,
        challenge,
        timestamp,
        client_challenge,
        negotiate_message,
        target_name,
    } = request;
    let target_info = protected_target_info(&challenge.target_info, target_name)?;
    let timestamp = av_timestamp(&challenge.target_info).unwrap_or(timestamp);
    let response_key_nt = ntlm_v2_hash(user, domain, password)?;
    let ntlm_v2 = ntlm_v2_response(
        &response_key_nt,
        challenge.server_challenge,
        client_challenge,
        timestamp,
        &target_info,
    )?;
    let session_base_key = ntlm_v2.session_base_key;
    let flags = NtlmsspNegotiateFlags(challenge.flags.0 & default_negotiate_flags().0);
    let (exported_session_key, encrypted_random_session_key) =
        if flags.contains(NtlmsspNegotiateFlags::KEY_EXCH) {
            let mut exported = [0; 16];
            getrandom::fill(&mut exported)
                .map_err(|_| SmbClientError::EncryptionNonceGenerationFailed)?;
            (exported, rc4(&session_base_key, &exported))
        } else {
            (session_base_key, Vec::new())
        };
    let mut authenticate = NtlmsspAuthenticate {
        flags,
        domain: domain.into(),
        user: user.into(),
        workstation: workstation.into(),
        ntlm_v2,
        encrypted_random_session_key,
        version: NTLM_VERSION,
        mic: [0; 16],
    };
    let zero_mic_message = authenticate.encode()?;
    let mut mic_input = Vec::with_capacity(
        negotiate_message.len() + challenge.raw_message.len() + zero_mic_message.len(),
    );
    mic_input.extend_from_slice(negotiate_message);
    mic_input.extend_from_slice(&challenge.raw_message);
    mic_input.extend_from_slice(&zero_mic_message);
    authenticate.mic = hmac_md5(&exported_session_key, &mic_input)?;
    Ok(NtlmsspAuthenticateResult {
        message: authenticate.encode()?,
        exported_session_key,
    })
}

fn protected_target_info(target_info: &[u8], target_name: &str) -> Result<Vec<u8>, SmbClientError> {
    let mut output = Vec::with_capacity(target_info.len() + target_name.len() * 2 + 32);
    let mut offset = 0usize;
    let mut flags = 0u32;
    while offset + 4 <= target_info.len() {
        let av_id = le_u16(target_info, offset);
        let length = le_u16(target_info, offset + 2) as usize;
        offset += 4;
        let value = checked_slice(target_info, offset, length)?;
        offset += length;
        if av_id == MSV_AV_EOL {
            break;
        }
        if av_id == MSV_AV_FLAGS {
            if value.len() == 4 {
                flags = le_u32(value, 0);
            }
            continue;
        }
        if matches!(av_id, MSV_AV_TARGET_NAME | MSV_AV_CHANNEL_BINDINGS) {
            continue;
        }
        push_av_pair(&mut output, av_id, value)?;
    }
    push_av_pair(
        &mut output,
        MSV_AV_FLAGS,
        &(flags | MSV_AV_FLAG_MIC_PRESENT).to_le_bytes(),
    )?;
    push_av_pair(
        &mut output,
        MSV_AV_TARGET_NAME,
        &encode_utf16le(target_name),
    )?;
    push_av_pair(&mut output, MSV_AV_CHANNEL_BINDINGS, &[0; 16])?;
    push_u16(&mut output, MSV_AV_EOL);
    push_u16(&mut output, 0);
    Ok(output)
}

fn av_timestamp(target_info: &[u8]) -> Option<u64> {
    let mut offset = 0usize;
    while offset + 4 <= target_info.len() {
        let av_id = le_u16(target_info, offset);
        let length = le_u16(target_info, offset + 2) as usize;
        offset += 4;
        let value = target_info.get(offset..offset.checked_add(length)?)?;
        if av_id == MSV_AV_TIMESTAMP && value.len() == 8 {
            return Some(u64::from_le_bytes(value.try_into().ok()?));
        }
        if av_id == MSV_AV_EOL {
            return None;
        }
        offset += length;
    }
    None
}

fn push_av_pair(output: &mut Vec<u8>, av_id: u16, value: &[u8]) -> Result<(), SmbClientError> {
    push_u16(output, av_id);
    push_u16(output, checked_u16_len("ntlmssp.av_pair", value.len())?);
    output.extend_from_slice(value);
    Ok(())
}

fn hmac_md5(key: &[u8], input: &[u8]) -> Result<[u8; 16], SmbClientError> {
    let mut mac = Hmac::<Md5>::new_from_slice(key).map_err(|_| SmbClientError::InvalidHmacKey)?;
    mac.update(input);
    Ok(mac.finalize().into_bytes().into())
}

fn rc4(key: &[u8], input: &[u8]) -> Vec<u8> {
    let mut state = [0u8; 256];
    for (index, value) in state.iter_mut().enumerate() {
        *value = index as u8;
    }
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + state[i] as usize + key[i % key.len()] as usize) & 0xff;
        state.swap(i, j);
    }
    let mut i = 0usize;
    j = 0;
    input
        .iter()
        .map(|byte| {
            i = (i + 1) & 0xff;
            j = (j + state[i] as usize) & 0xff;
            state.swap(i, j);
            byte ^ state[(state[i] as usize + state[j] as usize) & 0xff]
        })
        .collect()
}

fn validate_signature_and_type(bytes: &[u8], expected_type: u32) -> Result<(), SmbClientError> {
    if bytes.len() < 12 {
        return Err(SmbClientError::ShortFrame {
            expected: 12,
            actual: bytes.len(),
        });
    }
    if &bytes[..8] != NTLMSSP_SIGNATURE {
        return Err(SmbClientError::InvalidNtlmsspSignature);
    }
    let message_type = le_u32(bytes, 8);
    if message_type != expected_type {
        return Err(SmbClientError::UnknownNtlmsspMessageType { message_type });
    }
    Ok(())
}

fn security_buffer_slice(bytes: &[u8], field_offset: usize) -> Result<&[u8], SmbClientError> {
    let length = le_u16(bytes, field_offset) as usize;
    let offset = le_u32(bytes, field_offset + 4) as usize;
    checked_slice(bytes, offset, length)
}

fn push_security_buffer(
    bytes: &mut Vec<u8>,
    length: usize,
    offset: usize,
) -> Result<(), SmbClientError> {
    let length = checked_u16_len("ntlmssp.security_buffer.length", length)?;
    let offset = checked_u32_len("ntlmssp.security_buffer.offset", offset)?;
    push_u16(bytes, length);
    push_u16(bytes, length);
    push_u32(bytes, offset);
    Ok(())
}

fn checked_add(left: usize, right: usize) -> Result<usize, SmbClientError> {
    left.checked_add(right)
        .ok_or(SmbClientError::FieldTooLarge {
            field: "ntlmssp.payload",
            length: left,
            max: usize::MAX - right,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn negotiate_message_encodes_flags_and_security_buffers() {
        let message = NtlmsspNegotiate {
            flags: default_negotiate_flags(),
            domain: "DOMAIN".into(),
            workstation: "WORKSTATION".into(),
        };

        let encoded = message.encode().expect("encode");

        assert_eq!(&encoded[..8], NTLMSSP_SIGNATURE);
        assert_eq!(le_u32(&encoded, 8), NEGOTIATE_MESSAGE_TYPE);
        assert_eq!(le_u32(&encoded, 12), default_negotiate_flags().0);
        assert_eq!(
            security_buffer_slice(&encoded, 16).expect("domain"),
            encode_utf16le("DOMAIN")
        );
        assert_eq!(
            security_buffer_slice(&encoded, 24).expect("workstation"),
            encode_utf16le("WORKSTATION")
        );
    }

    #[test]
    fn challenge_decodes_target_and_target_info() {
        let target_name = encode_utf16le("DOMAIN");
        let target_info = b"av-pairs";
        let encoded = challenge_message(&target_name, target_info);

        let challenge = NtlmsspChallenge::decode(&encoded).expect("decode");

        assert_eq!(challenge.target_name, target_name);
        assert_eq!(challenge.flags, default_negotiate_flags());
        assert_eq!(challenge.server_challenge, [0x11; 8]);
        assert_eq!(challenge.target_info, target_info);
    }

    #[test]
    fn challenge_rejects_bad_signature() {
        let mut encoded = challenge_message(&[], &[]);
        encoded[0] = b'X';

        assert_eq!(
            NtlmsspChallenge::decode(&encoded),
            Err(SmbClientError::InvalidNtlmsspSignature)
        );
    }

    #[test]
    fn challenge_rejects_unexpected_type() {
        let mut encoded = challenge_message(&[], &[]);
        encoded[8..12].copy_from_slice(&AUTHENTICATE_MESSAGE_TYPE.to_le_bytes());

        assert_eq!(
            NtlmsspChallenge::decode(&encoded),
            Err(SmbClientError::UnknownNtlmsspMessageType {
                message_type: AUTHENTICATE_MESSAGE_TYPE
            })
        );
    }

    #[test]
    fn challenge_rejects_target_info_outside_message() {
        let mut encoded = challenge_message(&[], b"info");
        let invalid_offset = encoded.len() as u32 + 1;
        encoded[44..48].copy_from_slice(&invalid_offset.to_le_bytes());

        assert_eq!(
            NtlmsspChallenge::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: encoded.len() + 1,
                length: 4,
                available: encoded.len()
            })
        );
    }

    #[test]
    fn authenticate_message_encodes_ntlmv2_responses_and_identity() {
        let ntlm_v2 = NtlmV2Response {
            nt_challenge_response: b"nt-response".to_vec(),
            lm_challenge_response: [0x22; 24],
            session_base_key: [0x33; 16],
        };
        let message = NtlmsspAuthenticate {
            flags: default_negotiate_flags(),
            domain: "DOMAIN".into(),
            user: "User".into(),
            workstation: "WORKSTATION".into(),
            ntlm_v2: ntlm_v2.clone(),
            encrypted_random_session_key: b"session-key".to_vec(),
            version: NTLM_VERSION,
            mic: [0x44; 16],
        };

        let encoded = message.encode().expect("encode");

        assert_eq!(&encoded[..8], NTLMSSP_SIGNATURE);
        assert_eq!(le_u32(&encoded, 8), AUTHENTICATE_MESSAGE_TYPE);
        assert_eq!(
            security_buffer_slice(&encoded, 12).expect("lm"),
            ntlm_v2.lm_challenge_response
        );
        assert_eq!(
            security_buffer_slice(&encoded, 20).expect("nt"),
            ntlm_v2.nt_challenge_response
        );
        assert_eq!(
            security_buffer_slice(&encoded, 28).expect("domain"),
            encode_utf16le("DOMAIN")
        );
        assert_eq!(
            security_buffer_slice(&encoded, 36).expect("user"),
            encode_utf16le("User")
        );
        assert_eq!(
            security_buffer_slice(&encoded, 44).expect("workstation"),
            encode_utf16le("WORKSTATION")
        );
        assert_eq!(
            security_buffer_slice(&encoded, 52).expect("session key"),
            b"session-key"
        );
        assert_eq!(le_u32(&encoded, 60), default_negotiate_flags().0);
    }

    #[test]
    fn ntlm_v2_authenticate_message_uses_challenge_and_identity() {
        let challenge = NtlmsspChallenge {
            target_name: encode_utf16le("Domain"),
            flags: default_negotiate_flags(),
            server_challenge: hex_array_8("0123456789abcdef"),
            target_info: vec![0; 4],
            raw_message: challenge_message(&encode_utf16le("Domain"), &[0; 4]),
        };
        let client_challenge = hex_array_8("ffffff0011223344");
        let timestamp = 0x0123_4567_89ab_cdef;
        let result = ntlm_v2_authenticate_message(NtlmV2AuthenticateRequest {
            user: "User",
            domain: "Domain",
            password: "Password",
            workstation: "WORKSTATION",
            challenge: &challenge,
            timestamp,
            client_challenge,
            negotiate_message: b"negotiate",
            target_name: "cifs/server",
        })
        .expect("authenticate");
        assert_eq!(result.exported_session_key.len(), 16);
        assert_eq!(
            security_buffer_slice(&result.message, 12)
                .expect("lm")
                .len(),
            24
        );
        assert!(
            security_buffer_slice(&result.message, 20)
                .expect("nt")
                .len()
                > 48
        );
        assert_eq!(
            security_buffer_slice(&result.message, 52)
                .expect("session key")
                .len(),
            16
        );
        assert_ne!(
            &result.message[AUTHENTICATE_MIC_OFFSET..AUTHENTICATE_MIC_OFFSET + 16],
            &[0; 16]
        );
        assert_eq!(
            security_buffer_slice(&result.message, 28).expect("domain"),
            encode_utf16le("Domain")
        );
        assert_eq!(
            security_buffer_slice(&result.message, 36).expect("user"),
            encode_utf16le("User")
        );
        assert_eq!(
            security_buffer_slice(&result.message, 44).expect("workstation"),
            encode_utf16le("WORKSTATION")
        );
        assert_eq!(le_u32(&result.message, 60), default_negotiate_flags().0);
    }

    fn challenge_message(target_name: &[u8], target_info: &[u8]) -> Vec<u8> {
        let target_name_offset = CHALLENGE_FIXED_BYTES;
        let target_info_offset = target_name_offset + target_name.len();
        let mut bytes = Vec::with_capacity(target_info_offset + target_info.len());
        bytes.extend_from_slice(NTLMSSP_SIGNATURE);
        push_u32(&mut bytes, CHALLENGE_MESSAGE_TYPE);
        push_security_buffer(&mut bytes, target_name.len(), target_name_offset).expect("target");
        push_u32(&mut bytes, default_negotiate_flags().0);
        bytes.extend_from_slice(&[0x11; 8]);
        bytes.extend_from_slice(&[0; 8]);
        push_security_buffer(&mut bytes, target_info.len(), target_info_offset).expect("info");
        bytes.extend_from_slice(target_name);
        bytes.extend_from_slice(target_info);
        bytes
    }
}
