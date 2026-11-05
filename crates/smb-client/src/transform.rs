use crate::crypto::{sp800_108_counter_hmac_sha256, SMB311_PREAUTH_HASH_BYTES};
use crate::packet::negotiate::{Smb2Dialect, Smb2EncryptionCipher};
use crate::SmbClientError;
use aes::cipher::BlockCipherEncrypt;
use aes::Aes128;
use aes_gcm::aead::{AeadInOut, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce, Tag};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const SMB3_TRANSFORM_HEADER_BYTES: usize = 52;
const SMB3_TRANSFORM_PROTOCOL_ID: [u8; 4] = [0xfd, b'S', b'M', b'B'];
const SMB3_TRANSFORM_TAG_OFFSET: usize = 4;
const SMB3_TRANSFORM_NONCE_OFFSET: usize = 20;
const SMB3_TRANSFORM_AAD_OFFSET: usize = SMB3_TRANSFORM_NONCE_OFFSET;
const SMB3_TRANSFORM_NONCE_BYTES: usize = 12;
const SMB3_TRANSFORM_NONCE_FIELD_BYTES: usize = 16;
const SMB3_TRANSFORM_SIZE_OFFSET: usize = 36;
const SMB3_TRANSFORM_RESERVED_OFFSET: usize = 40;
const SMB3_TRANSFORM_FLAGS_OFFSET: usize = 42;
const SMB3_TRANSFORM_SESSION_ID_OFFSET: usize = 44;
const SMB3_TRANSFORM_ENCRYPTED: u16 = 0x0001;
const AES128_KEY_BYTES: usize = 16;
const AES256_KEY_BYTES: usize = 32;

#[derive(Clone, Eq, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct Smb3EncryptionKeys {
    #[zeroize(skip)]
    cipher: Smb2EncryptionCipher,
    client_to_server: Vec<u8>,
    server_to_client: Vec<u8>,
}

impl std::fmt::Debug for Smb3EncryptionKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Smb3EncryptionKeys")
            .finish_non_exhaustive()
    }
}

impl Smb3EncryptionKeys {
    pub fn new(
        client_to_server: [u8; AES128_KEY_BYTES],
        server_to_client: [u8; AES128_KEY_BYTES],
    ) -> Self {
        Self {
            cipher: Smb2EncryptionCipher::Aes128Gcm,
            client_to_server: client_to_server.to_vec(),
            server_to_client: server_to_client.to_vec(),
        }
    }

    fn for_cipher(
        cipher: Smb2EncryptionCipher,
        client_to_server: Vec<u8>,
        server_to_client: Vec<u8>,
    ) -> Self {
        Self {
            cipher,
            client_to_server,
            server_to_client,
        }
    }

    #[cfg(test)]
    pub(crate) fn reversed(&self) -> Self {
        Self::for_cipher(
            self.cipher,
            self.server_to_client.clone(),
            self.client_to_server.clone(),
        )
    }
}

pub fn derive_smb311_encryption_keys(
    session_base_key: &[u8],
    dialect: Smb2Dialect,
    preauth_integrity_hash: &[u8; SMB311_PREAUTH_HASH_BYTES],
) -> Result<Smb3EncryptionKeys, SmbClientError> {
    derive_smb3_encryption_keys(
        session_base_key,
        dialect,
        preauth_integrity_hash,
        Smb2EncryptionCipher::Aes128Gcm,
    )
}

pub fn derive_smb3_encryption_keys(
    session_base_key: &[u8],
    dialect: Smb2Dialect,
    preauth_integrity_hash: &[u8; SMB311_PREAUTH_HASH_BYTES],
    cipher: Smb2EncryptionCipher,
) -> Result<Smb3EncryptionKeys, SmbClientError> {
    if matches!(dialect, Smb2Dialect::Smb202 | Smb2Dialect::Smb210) {
        return Err(SmbClientError::UnsupportedEncryptionDialect);
    }
    let key_bytes = match cipher {
        Smb2EncryptionCipher::Aes256Gcm | Smb2EncryptionCipher::Aes256Ccm => AES256_KEY_BYTES,
        Smb2EncryptionCipher::Aes128Gcm | Smb2EncryptionCipher::Aes128Ccm => AES128_KEY_BYTES,
    };
    let (client_to_server, server_to_client) = match dialect {
        Smb2Dialect::Smb300 | Smb2Dialect::Smb302 => {
            if cipher != Smb2EncryptionCipher::Aes128Ccm {
                return Err(SmbClientError::UnsupportedEncryptionCipher {
                    cipher: cipher as u16,
                });
            }
            (
                sp800_108_counter_hmac_sha256(
                    session_base_key,
                    b"SMB2AESCCM",
                    b"ServerIn \0",
                    AES128_KEY_BYTES,
                )?,
                sp800_108_counter_hmac_sha256(
                    session_base_key,
                    b"SMB2AESCCM",
                    b"ServerOut\0",
                    AES128_KEY_BYTES,
                )?,
            )
        }
        Smb2Dialect::Smb311 => (
            sp800_108_counter_hmac_sha256(
                session_base_key,
                b"SMBC2SCipherKey",
                preauth_integrity_hash,
                key_bytes,
            )?,
            sp800_108_counter_hmac_sha256(
                session_base_key,
                b"SMBS2CCipherKey",
                preauth_integrity_hash,
                key_bytes,
            )?,
        ),
        Smb2Dialect::Smb202 | Smb2Dialect::Smb210 => {
            return Err(SmbClientError::UnsupportedEncryptionDialect)
        }
    };
    Ok(Smb3EncryptionKeys::for_cipher(
        cipher,
        client_to_server,
        server_to_client,
    ))
}

pub struct Smb3EncryptionContext {
    client_to_server: Smb3Cipher,
    server_to_client: Smb3Cipher,
    nonce_prefix: [u8; 8],
    next_nonce: u32,
}

enum Smb3Cipher {
    Aes128Ccm(Box<Aes128>),
    Aes128Gcm(Box<Aes128Gcm>),
    Aes256Gcm(Box<Aes256Gcm>),
}

impl Smb3EncryptionContext {
    pub fn new(keys: Smb3EncryptionKeys) -> Result<Self, SmbClientError> {
        let mut nonce_prefix = [0; 8];
        getrandom::fill(&mut nonce_prefix)
            .map_err(|_| SmbClientError::EncryptionNonceGenerationFailed)?;
        Self::with_nonce_prefix(keys, nonce_prefix)
    }

    fn with_nonce_prefix(
        keys: Smb3EncryptionKeys,
        nonce_prefix: [u8; 8],
    ) -> Result<Self, SmbClientError> {
        let client_to_server = cipher_from_key(keys.cipher, &keys.client_to_server)?;
        let server_to_client = cipher_from_key(keys.cipher, &keys.server_to_client)?;
        Ok(Self {
            client_to_server,
            server_to_client,
            nonce_prefix,
            next_nonce: 0,
        })
    }

    pub fn encrypt(
        &mut self,
        session_id: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, SmbClientError> {
        let original_size =
            u32::try_from(plaintext.len()).map_err(|_| SmbClientError::FieldTooLarge {
                field: "transform.original_message_size",
                length: plaintext.len(),
                max: u32::MAX as usize,
            })?;
        let counter = self.next_nonce;
        self.next_nonce = self
            .next_nonce
            .checked_add(1)
            .ok_or(SmbClientError::EncryptionNonceExhausted)?;

        let nonce_len = self.client_to_server.nonce_len();
        if nonce_len == 11 && counter > 0x00ff_ffff {
            return Err(SmbClientError::EncryptionNonceExhausted);
        }
        let mut nonce = [0; SMB3_TRANSFORM_NONCE_BYTES];
        nonce[..8].copy_from_slice(&self.nonce_prefix);
        if nonce_len == 11 {
            nonce[8..11].copy_from_slice(&counter.to_be_bytes()[1..]);
        } else {
            nonce[8..].copy_from_slice(&counter.to_be_bytes());
        }

        let mut packet = vec![0; SMB3_TRANSFORM_HEADER_BYTES];
        packet[..4].copy_from_slice(&SMB3_TRANSFORM_PROTOCOL_ID);
        packet
            [SMB3_TRANSFORM_NONCE_OFFSET..SMB3_TRANSFORM_NONCE_OFFSET + SMB3_TRANSFORM_NONCE_BYTES]
            .copy_from_slice(&nonce);
        packet[SMB3_TRANSFORM_SIZE_OFFSET..SMB3_TRANSFORM_SIZE_OFFSET + 4]
            .copy_from_slice(&original_size.to_le_bytes());
        packet[SMB3_TRANSFORM_FLAGS_OFFSET..SMB3_TRANSFORM_FLAGS_OFFSET + 2]
            .copy_from_slice(&SMB3_TRANSFORM_ENCRYPTED.to_le_bytes());
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET..SMB3_TRANSFORM_SESSION_ID_OFFSET + 8]
            .copy_from_slice(&session_id.to_le_bytes());
        packet.extend_from_slice(plaintext);

        let (header, ciphertext) = packet.split_at_mut(SMB3_TRANSFORM_HEADER_BYTES);
        let tag = encrypt_cipher(
            &self.client_to_server,
            &nonce[..nonce_len],
            &header[SMB3_TRANSFORM_AAD_OFFSET..],
            ciphertext,
        )?;
        header[SMB3_TRANSFORM_TAG_OFFSET..SMB3_TRANSFORM_TAG_OFFSET + tag.len()]
            .copy_from_slice(&tag);
        Ok(packet)
    }

    pub fn decrypt(
        &self,
        expected_session_id: u64,
        packet: &[u8],
    ) -> Result<Vec<u8>, SmbClientError> {
        let nonce_len = self.server_to_client.nonce_len();
        validate_transform_header(packet, expected_session_id, nonce_len)?;
        let header = &packet[..SMB3_TRANSFORM_HEADER_BYTES];
        let mut plaintext = packet[SMB3_TRANSFORM_HEADER_BYTES..].to_vec();
        let nonce = &header[SMB3_TRANSFORM_NONCE_OFFSET..SMB3_TRANSFORM_NONCE_OFFSET + nonce_len];
        let mut tag = [0; AES128_KEY_BYTES];
        tag.copy_from_slice(
            &header[SMB3_TRANSFORM_TAG_OFFSET..SMB3_TRANSFORM_TAG_OFFSET + AES128_KEY_BYTES],
        );
        decrypt_cipher(
            &self.server_to_client,
            nonce,
            &header[SMB3_TRANSFORM_AAD_OFFSET..],
            &mut plaintext,
            &tag,
        )?;
        Ok(plaintext)
    }

    #[cfg(test)]
    fn encrypt_server_to_client(
        &self,
        session_id: u64,
        nonce: [u8; SMB3_TRANSFORM_NONCE_BYTES],
        plaintext: &[u8],
    ) -> Vec<u8> {
        encode_with_cipher(&self.server_to_client, session_id, nonce, plaintext)
    }
}

fn cipher_from_key(cipher: Smb2EncryptionCipher, key: &[u8]) -> Result<Smb3Cipher, SmbClientError> {
    match cipher {
        Smb2EncryptionCipher::Aes128Gcm => Ok(Smb3Cipher::Aes128Gcm(Box::new(
            Aes128Gcm::new_from_slice(key).map_err(|_| SmbClientError::InvalidEncryptionKey)?,
        ))),
        Smb2EncryptionCipher::Aes256Gcm => Ok(Smb3Cipher::Aes256Gcm(Box::new(
            Aes256Gcm::new_from_slice(key).map_err(|_| SmbClientError::InvalidEncryptionKey)?,
        ))),
        Smb2EncryptionCipher::Aes128Ccm => Ok(Smb3Cipher::Aes128Ccm(Box::new(
            Aes128::new_from_slice(key).map_err(|_| SmbClientError::InvalidEncryptionKey)?,
        ))),
        Smb2EncryptionCipher::Aes256Ccm => Err(SmbClientError::UnsupportedEncryptionCipher {
            cipher: cipher as u16,
        }),
    }
}

impl Smb3Cipher {
    fn nonce_len(&self) -> usize {
        match self {
            Self::Aes128Ccm(_) => 11,
            Self::Aes128Gcm(_) | Self::Aes256Gcm(_) => 12,
        }
    }
}

fn encrypt_cipher(
    cipher: &Smb3Cipher,
    nonce: &[u8],
    aad: &[u8],
    plaintext: &mut [u8],
) -> Result<[u8; 16], SmbClientError> {
    if let Smb3Cipher::Aes128Ccm(cipher) = cipher {
        return ccm_encrypt(cipher, nonce, aad, plaintext);
    }
    let nonce: &Nonce<aes_gcm::aead::consts::U12> = nonce
        .try_into()
        .map_err(|_| SmbClientError::TransformAuthenticationFailed)?;
    let result = match cipher {
        Smb3Cipher::Aes128Ccm(_) => {
            return Err(SmbClientError::UnsupportedEncryptionCipher {
                cipher: Smb2EncryptionCipher::Aes128Ccm as u16,
            })
        }
        Smb3Cipher::Aes128Gcm(cipher) => {
            cipher.encrypt_inout_detached(nonce, aad, plaintext.into())
        }
        Smb3Cipher::Aes256Gcm(cipher) => {
            cipher.encrypt_inout_detached(nonce, aad, plaintext.into())
        }
    };
    result
        .map(Into::into)
        .map_err(|_| SmbClientError::TransformAuthenticationFailed)
}

fn decrypt_cipher(
    cipher: &Smb3Cipher,
    nonce: &[u8],
    aad: &[u8],
    ciphertext: &mut [u8],
    tag: &[u8; 16],
) -> Result<(), SmbClientError> {
    if let Smb3Cipher::Aes128Ccm(cipher) = cipher {
        return ccm_decrypt(cipher, nonce, aad, ciphertext, tag);
    }
    let nonce: &Nonce<aes_gcm::aead::consts::U12> = nonce
        .try_into()
        .map_err(|_| SmbClientError::TransformAuthenticationFailed)?;
    let tag: &Tag = tag
        .as_slice()
        .try_into()
        .map_err(|_| SmbClientError::TransformAuthenticationFailed)?;
    let result = match cipher {
        Smb3Cipher::Aes128Ccm(_) => {
            return Err(SmbClientError::UnsupportedEncryptionCipher {
                cipher: Smb2EncryptionCipher::Aes128Ccm as u16,
            })
        }
        Smb3Cipher::Aes128Gcm(cipher) => {
            cipher.decrypt_inout_detached(nonce, aad, ciphertext.into(), tag)
        }
        Smb3Cipher::Aes256Gcm(cipher) => {
            cipher.decrypt_inout_detached(nonce, aad, ciphertext.into(), tag)
        }
    };
    result.map_err(|_| SmbClientError::TransformAuthenticationFailed)
}

fn ccm_encrypt(
    cipher: &Aes128,
    nonce: &[u8],
    aad: &[u8],
    plaintext: &mut [u8],
) -> Result<[u8; 16], SmbClientError> {
    let mac = ccm_mac(cipher, nonce, aad, plaintext)?;
    let tag_mask = ccm_counter_block(cipher, nonce, 0)?;
    for (counter, chunk) in plaintext.chunks_mut(16).enumerate() {
        let stream = ccm_counter_block(cipher, nonce, (counter + 1) as u32)?;
        for (byte, mask) in chunk.iter_mut().zip(stream) {
            *byte ^= mask;
        }
    }
    let mut tag = [0; 16];
    for index in 0..16 {
        tag[index] = mac[index] ^ tag_mask[index];
    }
    Ok(tag)
}

fn ccm_decrypt(
    cipher: &Aes128,
    nonce: &[u8],
    aad: &[u8],
    ciphertext: &mut [u8],
    tag: &[u8; 16],
) -> Result<(), SmbClientError> {
    for (counter, chunk) in ciphertext.chunks_mut(16).enumerate() {
        let stream = ccm_counter_block(cipher, nonce, (counter + 1) as u32)?;
        for (byte, mask) in chunk.iter_mut().zip(stream) {
            *byte ^= mask;
        }
    }
    let mac = ccm_mac(cipher, nonce, aad, ciphertext)?;
    let tag_mask = ccm_counter_block(cipher, nonce, 0)?;
    let mut mismatch = 0u8;
    for index in 0..16 {
        mismatch |= tag[index] ^ mac[index] ^ tag_mask[index];
    }
    if mismatch == 0 {
        Ok(())
    } else {
        ciphertext.fill(0);
        Err(SmbClientError::TransformAuthenticationFailed)
    }
}

fn ccm_mac(
    cipher: &Aes128,
    nonce: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<[u8; 16], SmbClientError> {
    if nonce.len() != 11 || plaintext.len() > u32::MAX as usize || aad.len() >= 0xff00 {
        return Err(SmbClientError::TransformAuthenticationFailed);
    }
    let mut state = [0u8; 16];
    let mut b0 = [0u8; 16];
    b0[0] = 0x3b | (u8::from(!aad.is_empty()) * 0x40);
    b0[1..12].copy_from_slice(nonce);
    b0[12..].copy_from_slice(&(plaintext.len() as u32).to_be_bytes());
    ccm_mac_block(cipher, &mut state, &b0);

    if !aad.is_empty() {
        let mut encoded = Vec::with_capacity(2 + aad.len() + 15);
        encoded.extend_from_slice(&(aad.len() as u16).to_be_bytes());
        encoded.extend_from_slice(aad);
        encoded.resize(encoded.len().next_multiple_of(16), 0);
        for block in encoded.chunks_exact(16) {
            ccm_mac_block(cipher, &mut state, block);
        }
    }
    for chunk in plaintext.chunks(16) {
        let mut block = [0u8; 16];
        block[..chunk.len()].copy_from_slice(chunk);
        ccm_mac_block(cipher, &mut state, &block);
    }
    Ok(state)
}

fn ccm_mac_block(cipher: &Aes128, state: &mut [u8; 16], block: &[u8]) {
    for index in 0..16 {
        state[index] ^= block[index];
    }
    let mut encrypted = aes::Block::from(*state);
    cipher.encrypt_block(&mut encrypted);
    state.copy_from_slice(&encrypted);
}

fn ccm_counter_block(
    cipher: &Aes128,
    nonce: &[u8],
    counter: u32,
) -> Result<[u8; 16], SmbClientError> {
    if nonce.len() != 11 {
        return Err(SmbClientError::TransformAuthenticationFailed);
    }
    let mut block = aes::Block::default();
    block[0] = 3;
    block[1..12].copy_from_slice(nonce);
    block[12..].copy_from_slice(&counter.to_be_bytes());
    cipher.encrypt_block(&mut block);
    Ok(block.into())
}

pub fn is_smb3_transform(packet: &[u8]) -> bool {
    packet.starts_with(&SMB3_TRANSFORM_PROTOCOL_ID)
}

fn validate_transform_header(
    packet: &[u8],
    expected_session_id: u64,
    nonce_len: usize,
) -> Result<(), SmbClientError> {
    if packet.len() < SMB3_TRANSFORM_HEADER_BYTES {
        return Err(SmbClientError::ShortFrame {
            expected: SMB3_TRANSFORM_HEADER_BYTES,
            actual: packet.len(),
        });
    }
    if packet[..4] != SMB3_TRANSFORM_PROTOCOL_ID {
        return Err(SmbClientError::InvalidTransformProtocolId);
    }
    if packet[SMB3_TRANSFORM_NONCE_OFFSET + nonce_len
        ..SMB3_TRANSFORM_NONCE_OFFSET + SMB3_TRANSFORM_NONCE_FIELD_BYTES]
        .iter()
        .any(|byte| *byte != 0)
        || packet[SMB3_TRANSFORM_RESERVED_OFFSET..SMB3_TRANSFORM_RESERVED_OFFSET + 2] != [0; 2]
    {
        return Err(SmbClientError::InvalidTransformReserved);
    }
    let flags = u16::from_le_bytes([
        packet[SMB3_TRANSFORM_FLAGS_OFFSET],
        packet[SMB3_TRANSFORM_FLAGS_OFFSET + 1],
    ]);
    if flags != SMB3_TRANSFORM_ENCRYPTED {
        return Err(SmbClientError::InvalidTransformFlags { flags });
    }
    let session_id = u64::from_le_bytes([
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET],
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET + 1],
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET + 2],
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET + 3],
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET + 4],
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET + 5],
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET + 6],
        packet[SMB3_TRANSFORM_SESSION_ID_OFFSET + 7],
    ]);
    if session_id != expected_session_id {
        return Err(SmbClientError::TransformSessionMismatch {
            expected: expected_session_id,
            actual: session_id,
        });
    }
    let declared = u32::from_le_bytes([
        packet[SMB3_TRANSFORM_SIZE_OFFSET],
        packet[SMB3_TRANSFORM_SIZE_OFFSET + 1],
        packet[SMB3_TRANSFORM_SIZE_OFFSET + 2],
        packet[SMB3_TRANSFORM_SIZE_OFFSET + 3],
    ]) as usize;
    let actual = packet.len() - SMB3_TRANSFORM_HEADER_BYTES;
    if declared != actual {
        return Err(SmbClientError::TransformSizeMismatch { declared, actual });
    }
    Ok(())
}

#[cfg(test)]
fn encode_with_cipher(
    cipher: &Smb3Cipher,
    session_id: u64,
    nonce: [u8; SMB3_TRANSFORM_NONCE_BYTES],
    plaintext: &[u8],
) -> Vec<u8> {
    let mut packet = vec![0; SMB3_TRANSFORM_HEADER_BYTES];
    packet[..4].copy_from_slice(&SMB3_TRANSFORM_PROTOCOL_ID);
    packet[SMB3_TRANSFORM_NONCE_OFFSET..SMB3_TRANSFORM_NONCE_OFFSET + SMB3_TRANSFORM_NONCE_BYTES]
        .copy_from_slice(&nonce);
    packet[SMB3_TRANSFORM_SIZE_OFFSET..SMB3_TRANSFORM_SIZE_OFFSET + 4]
        .copy_from_slice(&(plaintext.len() as u32).to_le_bytes());
    packet[SMB3_TRANSFORM_FLAGS_OFFSET..SMB3_TRANSFORM_FLAGS_OFFSET + 2]
        .copy_from_slice(&SMB3_TRANSFORM_ENCRYPTED.to_le_bytes());
    packet[SMB3_TRANSFORM_SESSION_ID_OFFSET..SMB3_TRANSFORM_SESSION_ID_OFFSET + 8]
        .copy_from_slice(&session_id.to_le_bytes());
    packet.extend_from_slice(plaintext);
    let (header, ciphertext) = packet.split_at_mut(SMB3_TRANSFORM_HEADER_BYTES);
    let tag = encrypt_cipher(
        cipher,
        &nonce,
        &header[SMB3_TRANSFORM_AAD_OFFSET..],
        ciphertext,
    )
    .expect("test encryption");
    header[SMB3_TRANSFORM_TAG_OFFSET..SMB3_TRANSFORM_TAG_OFFSET + AES128_KEY_BYTES]
        .copy_from_slice(&tag);
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Smb3EncryptionKeys {
        Smb3EncryptionKeys::new([0x11; 16], [0x22; 16])
    }

    #[test]
    fn derives_directional_smb311_keys() {
        let session_key = b"0123456789abcdef";
        let preauth = [0x44; SMB311_PREAUTH_HASH_BYTES];
        let keys = derive_smb311_encryption_keys(session_key, Smb2Dialect::Smb311, &preauth)
            .expect("derive");

        assert_eq!(
            keys.client_to_server.to_vec(),
            sp800_108_counter_hmac_sha256(session_key, b"SMBC2SCipherKey", &preauth, 16)
                .expect("client key")
        );
        assert_eq!(
            keys.server_to_client.to_vec(),
            sp800_108_counter_hmac_sha256(session_key, b"SMBS2CCipherKey", &preauth, 16)
                .expect("server key")
        );
    }

    #[test]
    fn derives_smb300_ccm_keys_with_server_directions() {
        let session_key = b"0123456789abcdef";
        let keys = derive_smb3_encryption_keys(
            session_key,
            Smb2Dialect::Smb300,
            &[0; SMB311_PREAUTH_HASH_BYTES],
            Smb2EncryptionCipher::Aes128Ccm,
        )
        .expect("derive");

        assert_eq!(
            keys.client_to_server,
            sp800_108_counter_hmac_sha256(session_key, b"SMB2AESCCM", b"ServerIn \0", 16)
                .expect("server in")
        );
        assert_eq!(
            keys.server_to_client,
            sp800_108_counter_hmac_sha256(session_key, b"SMB2AESCCM", b"ServerOut\0", 16)
                .expect("server out")
        );
    }

    #[test]
    fn aes128_ccm_transform_round_trips_and_authenticates() {
        let keys = Smb3EncryptionKeys::for_cipher(
            Smb2EncryptionCipher::Aes128Ccm,
            vec![0x11; 16],
            vec![0x22; 16],
        );
        let reverse = keys.reversed();
        let mut client = Smb3EncryptionContext::with_nonce_prefix(keys, [1, 2, 3, 4, 5, 6, 7, 8])
            .expect("client");
        let server = Smb3EncryptionContext::with_nonce_prefix(reverse, [8, 7, 6, 5, 4, 3, 2, 1])
            .expect("server");
        let packet = client.encrypt(42, b"ccm payload").expect("encrypt");

        assert_eq!(
            server.decrypt(42, &packet).expect("decrypt"),
            b"ccm payload"
        );
        let mut tampered = packet;
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(server.decrypt(42, &tampered).is_err());
    }

    #[test]
    fn aes256_gcm_transform_round_trips() {
        let keys = Smb3EncryptionKeys::for_cipher(
            Smb2EncryptionCipher::Aes256Gcm,
            vec![0x31; 32],
            vec![0x42; 32],
        );
        let reverse = keys.reversed();
        let mut client = Smb3EncryptionContext::new(keys).expect("client");
        let server = Smb3EncryptionContext::new(reverse).expect("server");
        let packet = client.encrypt(7, b"gcm256 payload").expect("encrypt");
        assert_eq!(
            server.decrypt(7, &packet).expect("decrypt"),
            b"gcm256 payload"
        );
    }

    #[test]
    fn rejects_pre_smb311_encryption_derivation() {
        assert_eq!(
            derive_smb311_encryption_keys(b"key", Smb2Dialect::Smb210, &[0; 64]),
            Err(SmbClientError::UnsupportedEncryptionDialect)
        );
    }

    #[test]
    fn decrypts_authenticated_server_transform() {
        let context = Smb3EncryptionContext::with_nonce_prefix(keys(), [1, 2, 3, 4, 5, 6, 7, 8])
            .expect("context");
        let session_id = 0x0102_0304_0506_0708;
        let packet = context.encrypt_server_to_client(session_id, [0x55; 12], b"SMB payload");

        assert_eq!(
            context.decrypt(session_id, &packet).expect("decrypt"),
            b"SMB payload"
        );
    }

    #[test]
    fn transform_rejects_tag_and_authenticated_header_tampering() {
        let context = Smb3EncryptionContext::with_nonce_prefix(keys(), [1, 2, 3, 4, 5, 6, 7, 8])
            .expect("context");
        let session_id = 7;
        let packet = context.encrypt_server_to_client(session_id, [0x33; 12], b"payload");

        for offset in [SMB3_TRANSFORM_TAG_OFFSET, SMB3_TRANSFORM_FLAGS_OFFSET] {
            let mut tampered = packet.clone();
            tampered[offset] ^= 0x01;
            assert!(context.decrypt(session_id, &tampered).is_err());
        }
    }

    #[test]
    fn outbound_nonces_do_not_repeat() {
        let mut context =
            Smb3EncryptionContext::with_nonce_prefix(keys(), [1, 2, 3, 4, 5, 6, 7, 8])
                .expect("context");
        let first = context.encrypt(9, b"first").expect("first");
        let second = context.encrypt(9, b"second").expect("second");

        assert_ne!(
            &first[SMB3_TRANSFORM_NONCE_OFFSET
                ..SMB3_TRANSFORM_NONCE_OFFSET + SMB3_TRANSFORM_NONCE_BYTES],
            &second[SMB3_TRANSFORM_NONCE_OFFSET
                ..SMB3_TRANSFORM_NONCE_OFFSET + SMB3_TRANSFORM_NONCE_BYTES]
        );
    }

    #[test]
    fn aes128_gcm_matches_nist_zero_block_vector() {
        let cipher = Aes128Gcm::new((&[0; 16]).into());
        let mut block = [0; 16];
        let nonce: Nonce<aes_gcm::aead::consts::U12> = [0; 12].into();
        let tag = cipher
            .encrypt_inout_detached(&nonce, b"", block.as_mut_slice().into())
            .expect("encrypt");

        assert_eq!(hex(&block), "0388dace60b6a392f328c2b971b2fe78");
        assert_eq!(hex(&tag), "ab6e47d42cec13bdf53a67b21257bddf");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
