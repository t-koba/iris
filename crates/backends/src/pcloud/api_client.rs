use iris_path_service::{canonical_pcloud_api_base, DEFAULT_PCLOUD_API_BASE};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

pub const DEFAULT_PCLOUD_TRANSFER_IDLE_TIMEOUT_SECS: u64 = 30;
const DEFAULT_PCLOUD_TRANSFER_IDLE_TIMEOUT: Duration =
    Duration::from_secs(DEFAULT_PCLOUD_TRANSFER_IDLE_TIMEOUT_SECS);

mod auth;
mod ops;

/// Common envelope of pCloud API responses.
#[derive(Debug, Deserialize)]
pub struct PCloudResponse<T> {
    pub result: i32,
    pub error: Option<String>,
    #[serde(flatten)]
    pub data: Option<T>,
}

#[derive(Debug, Deserialize)]
pub struct PCloudMetadata {
    pub name: String,
    #[serde(rename = "folderid")]
    pub folder_id: Option<u64>,
    #[serde(rename = "fileid")]
    pub file_id: Option<u64>,
    #[serde(rename = "isfolder")]
    pub is_folder: bool,
    pub size: Option<u64>,
    #[serde(rename = "contenttype")]
    pub content_type: Option<String>,
    pub hash: Option<u64>,
    pub contents: Option<Vec<PCloudMetadata>>,
}

pub struct PCloudApiClient {
    pub(super) http: Client,
    pub(super) transfer_http: Client,
    pub(super) access_token: String,
    pub(super) api_base: String,
    pub(super) api_timeout: Duration,
    pub(super) transfer_idle_timeout: Duration,
}

const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn validate_transfer_idle_timeout(timeout: Duration) -> Result<(), &'static str> {
    if timeout.is_zero() || tokio::time::Instant::now().checked_add(timeout).is_none() {
        return Err("pCloud transfer idle timeout must be positive and representable");
    }
    Ok(())
}

fn build_http_client(read_timeout: Option<Duration>) -> Client {
    let builder = Client::builder()
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(8);
    let builder = match read_timeout {
        Some(timeout) => builder.read_timeout(timeout),
        None => builder,
    };
    builder.build().expect("failed to build pCloud HTTP client")
}

impl PCloudApiClient {
    /// Canonicalizes an API base using the shared pCloud origin policy.
    pub fn canonical_api_base(api_base: &str) -> Result<&'static str, &'static str> {
        canonical_pcloud_api_base(api_base)
    }

    fn build(access_token: String, api_base: String) -> Self {
        Self::build_with_timeouts(
            access_token,
            api_base,
            API_REQUEST_TIMEOUT,
            DEFAULT_PCLOUD_TRANSFER_IDLE_TIMEOUT,
        )
    }

    fn build_with_timeouts(
        access_token: String,
        api_base: String,
        api_timeout: Duration,
        transfer_idle_timeout: Duration,
    ) -> Self {
        crate::ensure_tls_crypto_provider();
        Self {
            http: build_http_client(None),
            transfer_http: build_http_client(Some(transfer_idle_timeout)),
            access_token,
            api_base,
            api_timeout,
            transfer_idle_timeout,
        }
    }

    pub fn new(access_token: String) -> Self {
        Self::build(access_token, DEFAULT_PCLOUD_API_BASE.to_string())
    }

    /// Builds a client only for the supported pCloud HTTPS API origins.
    pub fn with_base_url(access_token: String, api_base: String) -> Result<Self, &'static str> {
        Self::with_base_url_and_transfer_idle_timeout(
            access_token,
            api_base,
            DEFAULT_PCLOUD_TRANSFER_IDLE_TIMEOUT,
        )
    }

    /// Configures the maximum inactivity between transfer body progress events.
    pub(super) fn with_base_url_and_transfer_idle_timeout(
        access_token: String,
        api_base: String,
        transfer_idle_timeout: Duration,
    ) -> Result<Self, &'static str> {
        validate_transfer_idle_timeout(transfer_idle_timeout)?;
        let api_base = Self::canonical_api_base(&api_base)?.to_string();
        Ok(Self::build_with_timeouts(
            access_token,
            api_base,
            API_REQUEST_TIMEOUT,
            transfer_idle_timeout,
        ))
    }

    #[cfg(test)]
    pub(super) fn with_test_base_url(access_token: String, api_base: String) -> Self {
        Self::build(access_token, api_base)
    }

    #[cfg(test)]
    pub(super) fn with_test_base_url_and_timeouts(
        access_token: String,
        api_base: String,
        api_timeout: Duration,
        transfer_idle_timeout: Duration,
    ) -> Self {
        Self::build_with_timeouts(access_token, api_base, api_timeout, transfer_idle_timeout)
    }

    pub(super) fn transfer_http_client(&self) -> Client {
        self.transfer_http.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::PCloudApiClient;
    use std::time::Duration;

    #[test]
    fn transfer_idle_timeout_rejects_zero_and_unrepresentable_values() {
        for timeout in [Duration::ZERO, Duration::from_secs(u64::MAX)] {
            assert!(PCloudApiClient::with_base_url_and_transfer_idle_timeout(
                "synthetic-token".into(),
                "https://api.pcloud.com".into(),
                timeout,
            )
            .is_err());
        }
    }

    #[test]
    fn api_base_accepts_only_the_two_documented_https_origins() {
        assert_eq!(
            PCloudApiClient::canonical_api_base("https://api.pcloud.com"),
            Ok("https://api.pcloud.com")
        );
        assert_eq!(
            PCloudApiClient::canonical_api_base("https://eapi.pcloud.com/"),
            Ok("https://eapi.pcloud.com")
        );
        assert!(PCloudApiClient::with_base_url(
            "synthetic-token".to_owned(),
            "https://attacker.test".to_owned()
        )
        .is_err());
        for untrusted in [
            "http://api.pcloud.com",
            "https://api.pcloud.com.evil.test",
            "https://user@api.pcloud.com",
            "https://api.pcloud.com:443",
            "https://api.pcloud.com/path",
            "https://api.pcloud.com?next=evil",
            "https://api.pcloud.com#fragment",
            "https://attacker.test",
        ] {
            assert!(
                PCloudApiClient::canonical_api_base(untrusted).is_err(),
                "accepted untrusted API base: {untrusted}"
            );
        }
    }
}
