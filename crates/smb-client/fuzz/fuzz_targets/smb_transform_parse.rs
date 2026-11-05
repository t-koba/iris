#![no_main]

use iris_smb_client::{
    derive_smb3_encryption_keys, Smb2Dialect, Smb2EncryptionCipher, Smb3EncryptionContext,
};
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

static ENCRYPTION: OnceLock<Vec<Smb3EncryptionContext>> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    let encryption = ENCRYPTION.get_or_init(|| {
        let preauth_hash = [0x33; 64];
        [
            (Smb2Dialect::Smb300, Smb2EncryptionCipher::Aes128Ccm),
            (Smb2Dialect::Smb311, Smb2EncryptionCipher::Aes128Gcm),
            (Smb2Dialect::Smb311, Smb2EncryptionCipher::Aes256Gcm),
        ]
        .into_iter()
        .map(|(dialect, cipher)| {
            let keys = derive_smb3_encryption_keys(
                &[0x11; 16],
                dialect,
                &preauth_hash,
                cipher,
            )
            .expect("fuzz encryption key derivation");
            Smb3EncryptionContext::new(keys).expect("fuzz nonce initialization")
        })
        .collect()
    });
    let session_id = data
        .get(..8)
        .map(|bytes| {
            u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])
        })
        .unwrap_or(0);
    for context in encryption {
        let _ = context.decrypt(session_id, data);
    }
});
