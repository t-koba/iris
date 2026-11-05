use crate::packet::header::{Smb2Header, SMB2_HEADER_BYTES};
use crate::packet::negotiate::Smb2Dialect;
use crate::SmbClientError;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use aes::Aes128;
use sha2::{Digest, Sha256, Sha512};

const AES_CMAC_BLOCK_BYTES: usize = 16;
const AES_CMAC_RB: u8 = 0x87;
const SHA256_BLOCK_BYTES: usize = 64;
const SMB2_SIGNATURE_OFFSET: usize = 48;
const SMB2_SIGNATURE_BYTES: usize = 16;
pub const SMB311_PREAUTH_HASH_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SmbSigningKey {
    HmacSha256(Vec<u8>),
    AesCmac([u8; AES_CMAC_BLOCK_BYTES]),
}

impl SmbSigningKey {
    pub fn hmac_sha256(signing_key: impl Into<Vec<u8>>) -> Self {
        Self::HmacSha256(signing_key.into())
    }

    pub fn aes_cmac(signing_key: [u8; AES_CMAC_BLOCK_BYTES]) -> Self {
        Self::AesCmac(signing_key)
    }

    pub fn sign_packet(
        &self,
        packet: &mut [u8],
    ) -> Result<[u8; SMB2_SIGNATURE_BYTES], SmbClientError> {
        match self {
            Self::HmacSha256(signing_key) => sign_smb2_packet_hmac_sha256(signing_key, packet),
            Self::AesCmac(signing_key) => sign_smb2_packet_aes_cmac(signing_key, packet),
        }
    }

    pub fn verify_packet(&self, packet: &[u8]) -> Result<bool, SmbClientError> {
        match self {
            Self::HmacSha256(signing_key) => verify_smb2_packet_hmac_sha256(signing_key, packet),
            Self::AesCmac(signing_key) => verify_smb2_packet_aes_cmac(signing_key, packet),
        }
    }
}

pub fn smb311_initial_preauth_hash() -> [u8; SMB311_PREAUTH_HASH_BYTES] {
    [0; SMB311_PREAUTH_HASH_BYTES]
}

pub fn smb311_update_preauth_hash(
    previous_hash: &[u8; SMB311_PREAUTH_HASH_BYTES],
    message: &[u8],
) -> [u8; SMB311_PREAUTH_HASH_BYTES] {
    let mut hash = Sha512::new();
    hash.update(previous_hash);
    hash.update(message);
    hash.finalize().into()
}

pub fn smb311_preauth_hash_sequence<'a>(
    messages: impl IntoIterator<Item = &'a [u8]>,
) -> [u8; SMB311_PREAUTH_HASH_BYTES] {
    let mut hash = smb311_initial_preauth_hash();
    for message in messages {
        hash = smb311_update_preauth_hash(&hash, message);
    }
    hash
}

pub fn sp800_108_counter_hmac_sha256(
    key: &[u8],
    label: &[u8],
    context: &[u8],
    output_len: usize,
) -> Result<Vec<u8>, SmbClientError> {
    if key.is_empty() {
        return Err(SmbClientError::EmptyKdfKey);
    }
    if output_len == 0 {
        return Err(SmbClientError::InvalidKdfOutputLength { length: 0 });
    }
    let output_bits = output_len
        .checked_mul(8)
        .and_then(|bits| u32::try_from(bits).ok())
        .ok_or(SmbClientError::FieldTooLarge {
            field: "sp800_108.output_bits",
            length: output_len,
            max: u32::MAX as usize / 8,
        })?;
    let rounds = output_len.div_ceil(32);
    let mut output = Vec::with_capacity(rounds * 32);

    for counter in 1..=rounds {
        let counter = u32::try_from(counter).map_err(|_| SmbClientError::FieldTooLarge {
            field: "sp800_108.rounds",
            length: rounds,
            max: u32::MAX as usize,
        })?;
        let mut input = Vec::with_capacity(4 + label.len() + 1 + context.len() + 4);
        input.extend_from_slice(&counter.to_be_bytes());
        input.extend_from_slice(label);
        input.push(0);
        input.extend_from_slice(context);
        input.extend_from_slice(&output_bits.to_be_bytes());
        output.extend_from_slice(&hmac_sha256(key, &input));
    }

    output.truncate(output_len);
    Ok(output)
}

pub fn derive_smb2_signing_key(
    session_base_key: &[u8],
    dialect: Smb2Dialect,
    preauth_integrity_hash: &[u8; SMB311_PREAUTH_HASH_BYTES],
) -> Result<SmbSigningKey, SmbClientError> {
    match dialect {
        Smb2Dialect::Smb202 | Smb2Dialect::Smb210 => {
            validate_signing_key(session_base_key)?;
            Ok(SmbSigningKey::hmac_sha256(session_base_key.to_vec()))
        }
        Smb2Dialect::Smb300 | Smb2Dialect::Smb302 => Ok(SmbSigningKey::aes_cmac(
            derive_aes_cmac_signing_key(session_base_key, b"SMB2AESCMAC", b"SmbSign")?,
        )),
        Smb2Dialect::Smb311 => Ok(SmbSigningKey::aes_cmac(derive_aes_cmac_signing_key(
            session_base_key,
            b"SMBSigningKey",
            preauth_integrity_hash,
        )?)),
    }
}

fn derive_aes_cmac_signing_key(
    session_base_key: &[u8],
    label: &[u8],
    context: &[u8],
) -> Result<[u8; AES_CMAC_BLOCK_BYTES], SmbClientError> {
    let key =
        sp800_108_counter_hmac_sha256(session_base_key, label, context, AES_CMAC_BLOCK_BYTES)?;
    let mut signing_key = [0; AES_CMAC_BLOCK_BYTES];
    signing_key.copy_from_slice(&key);
    Ok(signing_key)
}

pub fn smb2_hmac_sha256_signature(
    signing_key: &[u8],
    packet: &[u8],
) -> Result<[u8; SMB2_SIGNATURE_BYTES], SmbClientError> {
    validate_signing_key(signing_key)?;
    validate_smb2_packet(packet)?;

    let mut signed_packet = packet.to_vec();
    signed_packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES].fill(0);
    let digest = hmac_sha256(signing_key, &signed_packet);
    let mut signature = [0; SMB2_SIGNATURE_BYTES];
    signature.copy_from_slice(&digest[..SMB2_SIGNATURE_BYTES]);
    Ok(signature)
}

pub fn sign_smb2_packet_hmac_sha256(
    signing_key: &[u8],
    packet: &mut [u8],
) -> Result<[u8; SMB2_SIGNATURE_BYTES], SmbClientError> {
    let signature = smb2_hmac_sha256_signature(signing_key, packet)?;
    packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES]
        .copy_from_slice(&signature);
    Ok(signature)
}

pub fn verify_smb2_packet_hmac_sha256(
    signing_key: &[u8],
    packet: &[u8],
) -> Result<bool, SmbClientError> {
    validate_smb2_packet(packet)?;
    let mut expected = [0; SMB2_SIGNATURE_BYTES];
    expected.copy_from_slice(
        &packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES],
    );
    let actual = smb2_hmac_sha256_signature(signing_key, packet)?;
    Ok(constant_time_eq(&expected, &actual))
}

pub fn smb2_aes_cmac_signature(
    signing_key: &[u8; AES_CMAC_BLOCK_BYTES],
    packet: &[u8],
) -> Result<[u8; SMB2_SIGNATURE_BYTES], SmbClientError> {
    validate_smb2_packet(packet)?;

    let mut signed_packet = packet.to_vec();
    signed_packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES].fill(0);
    Ok(aes_cmac(signing_key, &signed_packet))
}

pub fn sign_smb2_packet_aes_cmac(
    signing_key: &[u8; AES_CMAC_BLOCK_BYTES],
    packet: &mut [u8],
) -> Result<[u8; SMB2_SIGNATURE_BYTES], SmbClientError> {
    let signature = smb2_aes_cmac_signature(signing_key, packet)?;
    packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES]
        .copy_from_slice(&signature);
    Ok(signature)
}

pub fn verify_smb2_packet_aes_cmac(
    signing_key: &[u8; AES_CMAC_BLOCK_BYTES],
    packet: &[u8],
) -> Result<bool, SmbClientError> {
    validate_smb2_packet(packet)?;
    let mut expected = [0; SMB2_SIGNATURE_BYTES];
    expected.copy_from_slice(
        &packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES],
    );
    let actual = smb2_aes_cmac_signature(signing_key, packet)?;
    Ok(constant_time_eq(&expected, &actual))
}

fn validate_signing_key(signing_key: &[u8]) -> Result<(), SmbClientError> {
    if signing_key.is_empty() {
        return Err(SmbClientError::EmptySigningKey);
    }
    Ok(())
}

fn validate_smb2_packet(packet: &[u8]) -> Result<(), SmbClientError> {
    if packet.len() < SMB2_HEADER_BYTES {
        return Err(SmbClientError::ShortFrame {
            expected: SMB2_HEADER_BYTES,
            actual: packet.len(),
        });
    }
    Smb2Header::decode(packet)?;
    Ok(())
}

fn aes_cmac(key: &[u8; AES_CMAC_BLOCK_BYTES], data: &[u8]) -> [u8; AES_CMAC_BLOCK_BYTES] {
    let (subkey_one, subkey_two) = aes_cmac_subkeys(key);
    let block_count = data.len().div_ceil(AES_CMAC_BLOCK_BYTES).max(1);
    let last_block_complete = !data.is_empty() && data.len().is_multiple_of(AES_CMAC_BLOCK_BYTES);
    let last_block_start = (block_count - 1) * AES_CMAC_BLOCK_BYTES;
    let mut last_block = [0; AES_CMAC_BLOCK_BYTES];

    if last_block_complete {
        last_block
            .copy_from_slice(&data[last_block_start..last_block_start + AES_CMAC_BLOCK_BYTES]);
        xor_block(&mut last_block, &subkey_one);
    } else {
        let remainder = data.len().saturating_sub(last_block_start);
        last_block[..remainder].copy_from_slice(&data[last_block_start..]);
        last_block[remainder] = 0x80;
        xor_block(&mut last_block, &subkey_two);
    }

    let mut state = [0; AES_CMAC_BLOCK_BYTES];
    for block in data[..last_block_start].chunks_exact(AES_CMAC_BLOCK_BYTES) {
        for index in 0..AES_CMAC_BLOCK_BYTES {
            state[index] ^= block[index];
        }
        state = aes128_encrypt_block(key, &state);
    }

    xor_block(&mut state, &last_block);
    aes128_encrypt_block(key, &state)
}

fn aes_cmac_subkeys(
    key: &[u8; AES_CMAC_BLOCK_BYTES],
) -> ([u8; AES_CMAC_BLOCK_BYTES], [u8; AES_CMAC_BLOCK_BYTES]) {
    let zero = [0; AES_CMAC_BLOCK_BYTES];
    let subkey_one = aes_cmac_double(&aes128_encrypt_block(key, &zero));
    let subkey_two = aes_cmac_double(&subkey_one);
    (subkey_one, subkey_two)
}

fn aes_cmac_double(input: &[u8; AES_CMAC_BLOCK_BYTES]) -> [u8; AES_CMAC_BLOCK_BYTES] {
    let mut output = [0; AES_CMAC_BLOCK_BYTES];
    let mut carry = 0;
    for index in (0..AES_CMAC_BLOCK_BYTES).rev() {
        output[index] = (input[index] << 1) | carry;
        carry = input[index] >> 7;
    }
    if input[0] & 0x80 != 0 {
        output[AES_CMAC_BLOCK_BYTES - 1] ^= AES_CMAC_RB;
    }
    output
}

fn xor_block(left: &mut [u8; AES_CMAC_BLOCK_BYTES], right: &[u8; AES_CMAC_BLOCK_BYTES]) {
    for index in 0..AES_CMAC_BLOCK_BYTES {
        left[index] ^= right[index];
    }
}

fn aes128_encrypt_block(
    key: &[u8; AES_CMAC_BLOCK_BYTES],
    block: &[u8; AES_CMAC_BLOCK_BYTES],
) -> [u8; AES_CMAC_BLOCK_BYTES] {
    let cipher = Aes128::new(key.into());
    let mut encrypted = aes::Block::from(*block);
    cipher.encrypt_block(&mut encrypted);
    encrypted.into()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut key_block = [0; SHA256_BLOCK_BYTES];
    if key.len() > SHA256_BLOCK_BYTES {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36; SHA256_BLOCK_BYTES];
    let mut opad = [0x5c; SHA256_BLOCK_BYTES];
    for index in 0..SHA256_BLOCK_BYTES {
        ipad[index] ^= key_block[index];
        opad[index] ^= key_block[index];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8; SMB2_SIGNATURE_BYTES], right: &[u8; SMB2_SIGNATURE_BYTES]) -> bool {
    let mut diff = 0;
    for index in 0..SMB2_SIGNATURE_BYTES {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn smb311_preauth_hash_starts_with_zero_hash() {
        assert_eq!(
            smb311_initial_preauth_hash(),
            [0; SMB311_PREAUTH_HASH_BYTES]
        );
    }

    #[test]
    fn smb311_preauth_hash_matches_sha512_previous_hash_plus_message() {
        let previous = smb311_initial_preauth_hash();
        let message = b"SMB2 negotiate packet";
        let hash = smb311_update_preauth_hash(&previous, message);

        assert_eq!(
            hex(&hash),
            "6fff3d7ebad72eb54ce04c2fbf81fa12\
             0513ff9c7ab761029c2f1691a261a3b1\
             7c605e8834093bc8159f1d263b1462f2\
             b8c43f2e4a00a7599d65d244f7edaa1d"
        );
    }

    #[test]
    fn smb311_preauth_hash_sequence_is_order_sensitive() {
        let first = smb311_preauth_hash_sequence([b"negotiate".as_slice(), b"session-setup"]);
        let second = smb311_preauth_hash_sequence([b"session-setup".as_slice(), b"negotiate"]);

        assert_ne!(first, second);
    }

    #[test]
    fn sp800_108_counter_hmac_sha256_derives_single_block() {
        let key = b"session key";
        let label = b"SMBSigningKey";
        let context = b"preauth";
        let derived = sp800_108_counter_hmac_sha256(key, label, context, 32).expect("derive");

        let mut input = Vec::new();
        input.extend_from_slice(&1u32.to_be_bytes());
        input.extend_from_slice(label);
        input.push(0);
        input.extend_from_slice(context);
        input.extend_from_slice(&256u32.to_be_bytes());
        assert_eq!(derived, hmac_sha256(key, &input));
    }

    #[test]
    fn sp800_108_counter_hmac_sha256_truncates_multiple_blocks() {
        let key = b"session key";
        let label = b"SMBC2SCipherKey";
        let context = b"preauth-hash";
        let derived = sp800_108_counter_hmac_sha256(key, label, context, 48).expect("derive");

        let mut expected = Vec::new();
        for counter in [1u32, 2] {
            let mut input = Vec::new();
            input.extend_from_slice(&counter.to_be_bytes());
            input.extend_from_slice(label);
            input.push(0);
            input.extend_from_slice(context);
            input.extend_from_slice(&384u32.to_be_bytes());
            expected.extend_from_slice(&hmac_sha256(key, &input));
        }
        expected.truncate(48);
        assert_eq!(derived, expected);
    }

    #[test]
    fn sp800_108_counter_hmac_sha256_rejects_empty_key() {
        assert_eq!(
            sp800_108_counter_hmac_sha256(&[], b"label", b"context", 16),
            Err(SmbClientError::EmptyKdfKey)
        );
    }

    #[test]
    fn sp800_108_counter_hmac_sha256_rejects_zero_output_length() {
        assert_eq!(
            sp800_108_counter_hmac_sha256(b"key", b"label", b"context", 0),
            Err(SmbClientError::InvalidKdfOutputLength { length: 0 })
        );
    }

    #[test]
    fn derive_smb2_signing_key_uses_session_key_for_smb2() {
        let session_key = b"0123456789abcdef";
        let preauth = [0x44; SMB311_PREAUTH_HASH_BYTES];

        assert_eq!(
            derive_smb2_signing_key(session_key, Smb2Dialect::Smb210, &preauth).expect("derive"),
            SmbSigningKey::hmac_sha256(session_key.to_vec())
        );
    }

    #[test]
    fn derive_smb2_signing_key_uses_smb300_aes_cmac_context() {
        let session_key = b"0123456789abcdef";
        let preauth = [0x44; SMB311_PREAUTH_HASH_BYTES];

        assert_eq!(
            derive_smb2_signing_key(session_key, Smb2Dialect::Smb300, &preauth).expect("derive"),
            SmbSigningKey::aes_cmac(hex_array_16("9d93d58214de360016c2fe1422c33295"))
        );
    }

    #[test]
    fn derive_smb2_signing_key_uses_smb311_preauth_context() {
        let session_key = b"0123456789abcdef";
        let preauth = [0x44; SMB311_PREAUTH_HASH_BYTES];

        assert_eq!(
            derive_smb2_signing_key(session_key, Smb2Dialect::Smb311, &preauth).expect("derive"),
            SmbSigningKey::aes_cmac(hex_array_16("6a3ebff9a887da620fae67697722e3e1"))
        );
    }

    #[test]
    fn hmac_sha256_matches_rfc_4231_test_case_1() {
        let digest = hmac_sha256(&[0x0b; 20], b"Hi There");

        assert_eq!(
            hex(&digest),
            "b0344c61d8db38535ca8afceaf0bf12b\
             881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn aes_cmac_matches_rfc_4493_empty_message() {
        let key = hex_array_16("2b7e151628aed2a6abf7158809cf4f3c");

        assert_eq!(
            hex(&aes_cmac(&key, b"")),
            "bb1d6929e95937287fa37d129b756746"
        );
    }

    #[test]
    fn aes_cmac_matches_rfc_4493_single_block() {
        let key = hex_array_16("2b7e151628aed2a6abf7158809cf4f3c");
        let message = hex_bytes("6bc1bee22e409f96e93d7e117393172a");

        assert_eq!(
            hex(&aes_cmac(&key, &message)),
            "070a16b46b4d4144f79bdd9dd04a287c"
        );
    }

    #[test]
    fn smb2_hmac_signature_ignores_existing_signature_field() {
        let key = b"0123456789abcdef";
        let mut packet = packet_body();
        let signature = smb2_hmac_sha256_signature(key, &packet).expect("signature");
        packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES]
            .copy_from_slice(&[0xaa; SMB2_SIGNATURE_BYTES]);

        assert_eq!(
            smb2_hmac_sha256_signature(key, &packet).expect("signature"),
            signature
        );
    }

    #[test]
    fn sign_and_verify_smb2_hmac_packet() {
        let key = b"0123456789abcdef";
        let mut packet = packet_body();
        let signature = sign_smb2_packet_hmac_sha256(key, &mut packet).expect("sign");

        assert_eq!(
            &packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES],
            &signature
        );
        assert!(verify_smb2_packet_hmac_sha256(key, &packet).expect("verify"));
    }

    #[test]
    fn sign_and_verify_smb2_aes_cmac_packet() {
        let key = hex_array_16("2b7e151628aed2a6abf7158809cf4f3c");
        let mut packet = packet_body();
        let signature = sign_smb2_packet_aes_cmac(&key, &mut packet).expect("sign");

        assert_eq!(
            &packet[SMB2_SIGNATURE_OFFSET..SMB2_SIGNATURE_OFFSET + SMB2_SIGNATURE_BYTES],
            &signature
        );
        assert!(verify_smb2_packet_aes_cmac(&key, &packet).expect("verify"));
    }

    #[test]
    fn verify_smb2_hmac_packet_rejects_mutated_payload() {
        let key = b"0123456789abcdef";
        let mut packet = packet_body();
        sign_smb2_packet_hmac_sha256(key, &mut packet).expect("sign");
        let last = packet.len() - 1;
        packet[last] ^= 0xff;

        assert!(!verify_smb2_packet_hmac_sha256(key, &packet).expect("verify"));
    }

    #[test]
    fn smb2_hmac_rejects_empty_key() {
        let packet = packet_body();

        assert_eq!(
            smb2_hmac_sha256_signature(&[], &packet),
            Err(SmbClientError::EmptySigningKey)
        );
    }

    #[test]
    fn smb2_hmac_rejects_short_packet() {
        assert_eq!(
            smb2_hmac_sha256_signature(b"key", &[0; SMB2_HEADER_BYTES - 1]),
            Err(SmbClientError::ShortFrame {
                expected: SMB2_HEADER_BYTES,
                actual: SMB2_HEADER_BYTES - 1
            })
        );
    }

    fn packet_body() -> Vec<u8> {
        let mut packet = Smb2Header::new(Smb2Command::Read, 42).encode().to_vec();
        packet.extend_from_slice(b"body payload");
        packet
    }

    fn hex(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(DIGITS[(byte >> 4) as usize] as char);
            output.push(DIGITS[(byte & 0x0f) as usize] as char);
        }
        output
    }

    fn hex_array_16(input: &str) -> [u8; 16] {
        let bytes = hex_bytes(input);
        let mut array = [0; 16];
        array.copy_from_slice(&bytes);
        array
    }

    fn hex_bytes(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let hex = std::str::from_utf8(pair).expect("hex utf8");
                u8::from_str_radix(hex, 16).expect("hex byte")
            })
            .collect()
    }
}
