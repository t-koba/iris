use iris_path_service::{canonical_pcloud_api_base, DEFAULT_PCLOUD_API_BASE};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

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
    pub(super) access_token: String,
    pub(super) api_base: String,
}

fn build_http_client() -> Client {
    crate::ensure_tls_crypto_provider();
    Client::builder()
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(8)
        .build()
        .expect("failed to build pCloud HTTP client")
}

impl PCloudApiClient {
    /// Canonicalizes an API base using the shared pCloud origin policy.
    pub fn canonical_api_base(api_base: &str) -> Result<&'static str, &'static str> {
        canonical_pcloud_api_base(api_base)
    }

    fn build(access_token: String, api_base: String) -> Self {
        Self {
            http: build_http_client(),
            access_token,
            api_base,
        }
    }

    pub fn new(access_token: String) -> Self {
        Self::build(access_token, DEFAULT_PCLOUD_API_BASE.to_string())
    }

    /// Builds a client only for the supported pCloud HTTPS API origins.
    pub fn with_base_url(access_token: String, api_base: String) -> Result<Self, &'static str> {
        let api_base = Self::canonical_api_base(&api_base)?.to_string();
        Ok(Self::build(access_token, api_base))
    }

    #[cfg(test)]
    pub(super) fn with_test_base_url(access_token: String, api_base: String) -> Self {
        Self::build(access_token, api_base)
    }

    pub fn http_client(&self) -> Client {
        self.http.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::PCloudApiClient;

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
