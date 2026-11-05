use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

mod auth;
mod ops;

const PCLOUD_API_BASE: &str = "https://api.pcloud.com";

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
    pub fn new(access_token: String) -> Self {
        Self {
            http: build_http_client(),
            access_token,
            api_base: PCLOUD_API_BASE.to_string(),
        }
    }

    pub fn with_base_url(access_token: String, api_base: String) -> Self {
        Self {
            http: build_http_client(),
            access_token,
            api_base,
        }
    }

    pub fn http_client(&self) -> Client {
        self.http.clone()
    }
}
