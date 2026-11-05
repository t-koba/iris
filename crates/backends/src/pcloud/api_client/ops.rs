use super::{PCloudApiClient, PCloudMetadata, PCloudResponse};
use iris_core_domain::types::error::{BackendError, NetworkErrorDetail};
use reqwest::{multipart, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::pin::Pin;
use tokio::io::AsyncRead;
use tokio_util::io::ReaderStream;

impl PCloudApiClient {
    pub async fn list_folder(&self, folder_id: u64) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata("listfolder", &[("folderid", folder_id.to_string())])
            .await
    }

    pub async fn stat_file(&self, file_id: u64) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata("stat", &[("fileid", file_id.to_string())])
            .await
    }

    pub async fn get_file_link(&self, file_id: u64) -> Result<String, BackendError> {
        let data: PCloudFileLinkResponse = self
            .call("getfilelink", &[("fileid", file_id.to_string())])
            .await?
            .ok_or_else(|| internal_error("missing file link data"))?;
        let host = data
            .hosts
            .first()
            .ok_or_else(|| internal_error("no download host available"))?;
        Ok(format!("https://{}{}", host, data.path))
    }

    pub async fn copy_file(
        &self,
        file_id: u64,
        to_folder_id: u64,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "copyfile",
            &[
                ("fileid", file_id.to_string()),
                ("tofolderid", to_folder_id.to_string()),
            ],
        )
        .await
    }

    pub async fn copy_folder(
        &self,
        folder_id: u64,
        to_folder_id: u64,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "copyfolder",
            &[
                ("folderid", folder_id.to_string()),
                ("tofolderid", to_folder_id.to_string()),
            ],
        )
        .await
    }

    pub async fn rename_file(
        &self,
        file_id: u64,
        to_folder_id: u64,
        new_name: &str,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "renamefile",
            &[
                ("fileid", file_id.to_string()),
                ("tofolderid", to_folder_id.to_string()),
                ("toname", new_name.to_string()),
            ],
        )
        .await
    }

    pub async fn rename_folder(
        &self,
        folder_id: u64,
        to_folder_id: u64,
        new_name: &str,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "renamefolder",
            &[
                ("folderid", folder_id.to_string()),
                ("tofolderid", to_folder_id.to_string()),
                ("toname", new_name.to_string()),
            ],
        )
        .await
    }

    pub async fn delete_file(&self, file_id: u64) -> Result<(), BackendError> {
        self.call::<serde_json::Value>("deletefile", &[("fileid", file_id.to_string())])
            .await
            .map(drop)
    }

    pub async fn delete_folder_recursive(&self, folder_id: u64) -> Result<(), BackendError> {
        self.call::<serde_json::Value>(
            "deletefolderrecursive",
            &[("folderid", folder_id.to_string())],
        )
        .await
        .map(drop)
    }

    pub async fn create_folder(
        &self,
        parent_folder_id: u64,
        name: &str,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "createfolder",
            &[
                ("folderid", parent_folder_id.to_string()),
                ("name", name.to_string()),
            ],
        )
        .await
    }

    pub async fn get_thumb(
        &self,
        file_id: u64,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, BackendError> {
        let response = self
            .request("getthumb")
            .query(&[
                ("fileid", file_id.to_string()),
                ("size", format!("{width}x{height}")),
            ])
            .send()
            .await
            .map_err(|e| network_error("getthumb", e))?;
        if !response.status().is_success() {
            return Err(BackendError::Unsupported {
                operation: "get_thumbnail for this file type",
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;
        Ok(bytes.to_vec())
    }

    pub async fn upload_file_to_folder(
        &self,
        folder_id: u64,
        file_name: &str,
        data: Vec<u8>,
    ) -> Result<PCloudMetadata, BackendError> {
        self.upload(
            folder_id,
            multipart::Part::bytes(data).file_name(file_name.to_string()),
        )
        .await
    }

    pub async fn upload_stream_to_folder(
        &self,
        folder_id: u64,
        file_name: &str,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        content_length: Option<u64>,
    ) -> Result<PCloudMetadata, BackendError> {
        let body = reqwest::Body::wrap_stream(ReaderStream::new(reader));
        let part = match content_length {
            Some(length) => multipart::Part::stream_with_length(body, length),
            None => multipart::Part::stream(body),
        };
        self.upload(folder_id, part.file_name(file_name.to_string()))
            .await
    }

    async fn upload(
        &self,
        folder_id: u64,
        part: multipart::Part,
    ) -> Result<PCloudMetadata, BackendError> {
        let response = self
            .http
            .post(format!("{}/uploadfile", self.api_base))
            .query(&[
                ("access_token", self.access_token.clone()),
                ("folderid", folder_id.to_string()),
            ])
            .multipart(multipart::Form::new().part("file", part))
            .send()
            .await
            .map_err(|e| network_error("uploadfile", e))?;
        let body = parse_response::<serde_json::Value>("uploadfile", response).await?;
        extract_uploaded_metadata(&body.unwrap_or_default())
    }

    fn request(&self, method: &str) -> RequestBuilder {
        self.http
            .get(format!("{}/{method}", self.api_base))
            .query(&[("access_token", self.access_token.as_str())])
    }

    /// Calls `method` and returns the body of a successful response.
    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<Option<T>, BackendError> {
        let response = self
            .request(method)
            .query(params)
            .send()
            .await
            .map_err(|e| network_error(method, e))?;
        parse_response(method, response).await
    }

    async fn call_metadata(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<PCloudMetadata, BackendError> {
        self.call::<PCloudMetadataResponse>(method, params)
            .await?
            .map(|data| data.metadata)
            .ok_or_else(|| internal_error(&format!("missing metadata in pCloud {method} response")))
    }
}

async fn parse_response<T: DeserializeOwned>(
    method: &str,
    response: reqwest::Response,
) -> Result<Option<T>, BackendError> {
    let body: PCloudResponse<T> = response
        .json()
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    PCloudApiClient::ensure_success(method, &body)?;
    Ok(body.data)
}

fn network_error(method: &str, error: reqwest::Error) -> BackendError {
    BackendError::Network(NetworkErrorDetail {
        message: format!("pCloud {method} failed: {error}"),
        is_timeout: error.is_timeout(),
    })
}

fn internal_error(message: &str) -> BackendError {
    BackendError::Internal(Box::new(std::io::Error::other(message.to_string())))
}

#[derive(Debug, Deserialize)]
struct PCloudMetadataResponse {
    metadata: PCloudMetadata,
}

#[derive(Debug, Deserialize)]
struct PCloudFileLinkResponse {
    hosts: Vec<String>,
    path: String,
}

fn extract_uploaded_metadata(body: &serde_json::Value) -> Result<PCloudMetadata, BackendError> {
    let metadata = match body.get("metadata") {
        Some(serde_json::Value::Array(items)) => items.first(),
        other => other,
    }
    .ok_or_else(|| internal_error("missing uploaded metadata in pCloud upload response"))?;
    serde_json::from_value(metadata.clone()).map_err(|e| BackendError::Internal(Box::new(e)))
}
