//! `BackendAdapter` implementations: the local file system (NFS mounts included), SMB, S3
//! and pCloud.

pub mod common;
pub mod localfs;
pub mod pcloud;
pub mod registry;
pub mod s3;
pub mod smb;

/// Makes ring the process-wide rustls crypto provider; call before building a reqwest client.
///
/// reqwest is built without a bundled provider so only one crypto library is linked (ring, as
/// used by the S3 client and SSH); building a client without a provider would panic.
pub fn ensure_tls_crypto_provider() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        // Err only means a provider is already installed, which is just as good.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn https_clients_build_with_the_ring_provider() {
        super::ensure_tls_crypto_provider();
        // Without an installed provider reqwest panics here ("No rustls crypto provider").
        assert!(reqwest::Client::builder().build().is_ok());
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
