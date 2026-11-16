use crate::pcloud::api_client::{
    validate_transfer_idle_timeout, PCloudApiClient, PCloudMetadata,
    DEFAULT_PCLOUD_TRANSFER_IDLE_TIMEOUT_SECS,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use iris_auth::credential_manager::CredentialManager;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::{BackendError, NetworkErrorDetail};
use iris_core_domain::types::list::{
    EntryNameOnly, EntryWithMeta, ListCursor, ListPage, ListedEntry,
};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use iris_core_domain::types::thumbnail::{
    ImageFormat, ThumbnailHint, ThumbnailResult, ThumbnailSource,
};
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::sync::OnceCell;

/// pCloud adapter.
pub struct PCloudAdapter {
    api: OnceCell<PCloudApiClient>,
    backend_id: BackendId,
    ticket_counter: AtomicU64,
    stored_credentials: Option<PCloudStoredCredentials>,
}

struct PCloudStoredCredentials {
    account_id: String,
    api_base: String,
    transfer_idle_timeout: Duration,
}

impl PCloudAdapter {
    pub fn new(access_token: String, backend_id: BackendId) -> Self {
        Self {
            api: OnceCell::from(PCloudApiClient::new(access_token)),
            backend_id,
            ticket_counter: AtomicU64::new(1),
            stored_credentials: None,
        }
    }

    pub fn new_with_base(
        access_token: String,
        backend_id: BackendId,
        api_base: String,
    ) -> Result<Self, &'static str> {
        Self::new_with_base_and_transfer_idle_timeout(
            access_token,
            backend_id,
            api_base,
            Duration::from_secs(DEFAULT_PCLOUD_TRANSFER_IDLE_TIMEOUT_SECS),
        )
    }

    pub fn new_with_base_and_transfer_idle_timeout(
        access_token: String,
        backend_id: BackendId,
        api_base: String,
        transfer_idle_timeout: Duration,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            api: OnceCell::from(PCloudApiClient::with_base_url_and_transfer_idle_timeout(
                access_token,
                api_base,
                transfer_idle_timeout,
            )?),
            backend_id,
            ticket_counter: AtomicU64::new(1),
            stored_credentials: None,
        })
    }

    #[cfg(test)]
    fn new_with_test_base(access_token: String, backend_id: BackendId, api_base: String) -> Self {
        Self {
            api: OnceCell::from(PCloudApiClient::with_test_base_url(access_token, api_base)),
            backend_id,
            ticket_counter: AtomicU64::new(1),
            stored_credentials: None,
        }
    }

    pub fn new_with_stored_credentials(
        backend_id: BackendId,
        account_id: String,
        api_base: String,
    ) -> Result<Self, &'static str> {
        Self::new_with_stored_credentials_and_transfer_idle_timeout(
            backend_id,
            account_id,
            api_base,
            Duration::from_secs(DEFAULT_PCLOUD_TRANSFER_IDLE_TIMEOUT_SECS),
        )
    }

    pub fn new_with_stored_credentials_and_transfer_idle_timeout(
        backend_id: BackendId,
        account_id: String,
        api_base: String,
        transfer_idle_timeout: Duration,
    ) -> Result<Self, &'static str> {
        validate_transfer_idle_timeout(transfer_idle_timeout)?;
        let api_base = PCloudApiClient::canonical_api_base(&api_base)?.to_string();
        Ok(Self {
            api: OnceCell::new(),
            backend_id,
            ticket_counter: AtomicU64::new(1),
            stored_credentials: Some(PCloudStoredCredentials {
                account_id,
                api_base,
                transfer_idle_timeout,
            }),
        })
    }

    async fn api(&self) -> Result<&PCloudApiClient, BackendError> {
        self.api
            .get_or_try_init(|| async {
                let credentials = self.stored_credentials.as_ref().ok_or_else(|| {
                    BackendError::Internal(Box::new(io::Error::other(
                        "pCloud API client is not configured",
                    )))
                })?;
                let backend_id = self.backend_id.0.clone();
                let account_id = credentials.account_id.clone();
                let token = tokio::task::spawn_blocking(move || {
                    CredentialManager::new().load_token(&backend_id, &account_id)
                })
                .await
                .map_err(|error| BackendError::Internal(Box::new(error)))?
                .map_err(|error| {
                    BackendError::Auth(iris_core_domain::types::error::AuthErrorDetail {
                        message: error.to_string(),
                        is_token_expired: false,
                    })
                })?;
                PCloudApiClient::with_base_url_and_transfer_idle_timeout(
                    token.access_token,
                    credentials.api_base.clone(),
                    credentials.transfer_idle_timeout,
                )
                .map_err(|message| BackendError::Internal(Box::new(io::Error::other(message))))
            })
            .await
    }

    fn next_ticket(&self, kind: OperationKind) -> OperationTicket {
        let id = self.ticket_counter.fetch_add(1, Ordering::Relaxed);
        OperationTicket {
            id: TicketId(id),
            kind,
        }
    }

    fn parse_id(object: &ObjectRef) -> Result<(bool, u64), BackendError> {
        // stable_id format: "folder:{id}" or "file:{id}"
        let sid = &object.key.stable_id.0;
        if let Some(id_str) = sid.strip_prefix("folder:") {
            let id = id_str
                .parse::<u64>()
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            Ok((true, id))
        } else if let Some(id_str) = sid.strip_prefix("file:") {
            let id = id_str
                .parse::<u64>()
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            Ok((false, id))
        } else {
            Err(BackendError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid pCloud stable_id: {}", sid),
            ))))
        }
    }

    fn metadata_to_object_ref(
        &self,
        meta: &PCloudMetadata,
        parent_stable_id: Option<StableId>,
        parent_display_path: &str,
    ) -> Result<ObjectRef, BackendError> {
        let stable_id = if meta.is_folder {
            let folder_id = meta.folder_id.ok_or_else(|| {
                BackendError::Internal(Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "pCloud metadata for folder '{}' is missing folder_id",
                        meta.name
                    ),
                )))
            })?;
            StableId(format!("folder:{}", folder_id))
        } else {
            let file_id = meta.file_id.ok_or_else(|| {
                BackendError::Internal(Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "pCloud metadata for file '{}' is missing file_id",
                        meta.name
                    ),
                )))
            })?;
            StableId(format!("file:{}", file_id))
        };

        Ok(ObjectRef {
            key: ObjectKey {
                backend_id: self.backend_id.clone(),
                stable_id,
            },
            parent_stable_id,
            display_path: if parent_display_path.is_empty() {
                meta.name.clone()
            } else {
                format!("{}/{}", parent_display_path, meta.name)
            },
        })
    }

    fn parse_folder_id_from_stable_id(sid: &StableId) -> Option<u64> {
        sid.0
            .strip_prefix("folder:")
            .and_then(|v| v.parse::<u64>().ok())
    }

    fn split_parent_and_name(display_path: &str) -> Result<(String, String), BackendError> {
        let p = Path::new(display_path);
        let file_name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "invalid destination path for pCloud upload: '{}'",
                        display_path
                    ),
                )))
            })?;
        let parent = p
            .parent()
            .map(|v| v.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok((parent, file_name))
    }

    async fn resolve_write_target(
        &self,
        dst: &ObjectRef,
        on_conflict: &ConflictResolution,
    ) -> Result<(u64, String), BackendError> {
        let (parent_path, requested_name) = Self::split_parent_and_name(&dst.display_path)?;

        let folder_id = if let Some(parent_sid) = &dst.parent_stable_id {
            Self::parse_folder_id_from_stable_id(parent_sid).ok_or_else(|| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "invalid parent stable id for pCloud folder: {}",
                        parent_sid.0
                    ),
                )))
            })?
        } else {
            Self::parse_folder_id_from_stable_id(&dst.key.stable_id).ok_or_else(|| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "destination is not a pCloud folder: {}",
                        dst.key.stable_id.0
                    ),
                )))
            })?
        };

        let folder_meta = self.api().await?.list_folder(folder_id).await?;
        let mut existing_by_name = std::collections::HashMap::new();
        for item in folder_meta.contents.unwrap_or_default() {
            existing_by_name.insert(item.name.clone(), item);
        }

        if !existing_by_name.contains_key(&requested_name) {
            return Ok((folder_id, requested_name));
        }

        match on_conflict {
            // Files use the provider's replacement operation; folder replacement has
            // no established atomic API and must not be emulated by deleting the target.
            ConflictResolution::Overwrite if !existing_by_name[&requested_name].is_folder => {
                Ok((folder_id, requested_name))
            }
            ConflictResolution::Rename => {
                let stem = Path::new(&requested_name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| requested_name.clone());
                let ext = Path::new(&requested_name)
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();

                let mut index = 1u32;
                loop {
                    let candidate = format!("{} ({}){}", stem, index, ext);
                    if !existing_by_name.contains_key(&candidate) {
                        return Ok((folder_id, candidate));
                    }
                    index += 1;
                }
            }
            ConflictResolution::Overwrite | ConflictResolution::Skip | ConflictResolution::Fail => {
                let conflict_path = if parent_path.is_empty() {
                    requested_name.clone()
                } else {
                    format!("{}/{}", parent_path, requested_name)
                };
                Err(BackendError::Conflict {
                    object: ObjectRef {
                        key: ObjectKey {
                            backend_id: self.backend_id.clone(),
                            stable_id: StableId("unknown".to_string()),
                        },
                        parent_stable_id: dst.parent_stable_id.clone(),
                        display_path: conflict_path,
                    },
                })
            }
        }
    }
}

#[async_trait]
impl BackendAdapter for PCloudAdapter {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        // Listing the root folder (id 0) checks the connection.
        self.api().await?.list_folder(0).await?;
        Ok(BackendSession {
            backend_id: self.backend_id.clone(),
            display_name: "pCloud".to_string(),
        })
    }

    async fn list_names(
        &self,
        dir: &ObjectRef,
        _cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        let (is_folder, id) = Self::parse_id(dir)?;
        if !is_folder {
            return Err(BackendError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cannot list a file",
            ))));
        }

        // `listfolder` already returns the whole folder; this backend never emits a cursor.
        let metadata = self.api().await?.list_folder(id).await?;
        let parent_stable_id = dir.key.stable_id.clone();
        let entries = metadata
            .contents
            .unwrap_or_default()
            .iter()
            .map(|item| {
                let name = item.name.clone();
                let obj_ref = self.metadata_to_object_ref(
                    item,
                    Some(parent_stable_id.clone()),
                    &dir.display_path,
                )?;
                Ok(ListedEntry::WithMeta(EntryWithMeta {
                    name: EntryNameOnly {
                        object_ref: obj_ref,
                        name,
                        is_dir: item.is_folder,
                        is_symlink: false,
                        is_inaccessible: false,
                    },
                    size: item.size,
                    mtime: None, // pCloud dates are strings and are not parsed
                    ctime: None,
                }))
            })
            .collect::<Result<Vec<_>, BackendError>>()?;

        Ok(ListPage {
            entries,
            next_cursor: None,
        })
    }

    async fn stat(&self, object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        let (is_folder, id) = Self::parse_id(object)?;
        if is_folder {
            // Fails when the folder does not exist.
            self.api().await?.list_folder(id).await?;
            return Ok(MetadataResult {
                level: MetadataLevel::L1,
                is_dir: true,
                size: None,
                mtime: None,
                ctime: None,
                content_type: None,
                etag: None,
            });
        }

        let metadata = self.api().await?.stat_file(id).await?;
        Ok(MetadataResult {
            level: MetadataLevel::L1,
            is_dir: false,
            size: metadata.size,
            mtime: None,
            ctime: None,
            content_type: metadata.content_type.clone(),
            etag: metadata.hash.map(|h| h.to_string()),
        })
    }

    async fn read(
        &self,
        object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        let (_, file_id) = Self::parse_id(object)?;
        let download_url = self.api().await?.get_file_link(file_id).await?;

        download_read(
            self.api().await?.transfer_http_client(),
            &download_url,
            range,
        )
        .await
    }

    async fn write_object(
        &self,
        dst: &ObjectRef,
        data: Vec<u8>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let (folder_id, file_name) = self.resolve_write_target(dst, &on_conflict).await?;
        self.api()
            .await?
            .upload_file_to_folder(folder_id, &file_name, data)
            .await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn write_from_reader(
        &self,
        dst: &ObjectRef,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        content_length: Option<u64>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let (folder_id, file_name) = self.resolve_write_target(dst, &on_conflict).await?;
        self.api()
            .await?
            .upload_stream_to_folder(folder_id, &file_name, reader, content_length)
            .await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn mkdir(&self, parent: &ObjectRef, name: &str) -> Result<ObjectRef, BackendError> {
        let (_, parent_id) = Self::parse_id(parent)?;
        let metadata = self.api().await?.create_folder(parent_id, name).await?;
        self.metadata_to_object_ref(
            &metadata,
            Some(parent.key.stable_id.clone()),
            &parent.display_path,
        )
    }

    async fn copy(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: CopyOptions,
    ) -> Result<OperationTicket, BackendError> {
        if src.key == dst.key {
            return Ok(self.next_ticket(OperationKind::Copy));
        }
        let (is_folder, source_id) = Self::parse_id(src)?;
        let (to_folder_id, target_name) =
            self.resolve_write_target(dst, &options.on_conflict).await?;
        if is_folder {
            let copied = self
                .api()
                .await?
                .copy_folder(source_id, to_folder_id)
                .await?;
            if copied.name != target_name {
                self.api()
                    .await?
                    .rename_folder(
                        copied.folder_id.ok_or_else(|| {
                            BackendError::Internal(Box::new(std::io::Error::other(
                                "missing folder_id in copied pCloud folder",
                            )))
                        })?,
                        to_folder_id,
                        &target_name,
                    )
                    .await?;
            }
        } else {
            self.api()
                .await?
                .copy_file(source_id, to_folder_id, &target_name)
                .await?;
        }
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn move_object(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
        if src.key == dst.key {
            return Ok(self.next_ticket(OperationKind::Move));
        }
        let (is_folder, source_id) = Self::parse_id(src)?;
        let (to_folder_id, target_name) =
            self.resolve_write_target(dst, &options.on_conflict).await?;
        if is_folder {
            self.api()
                .await?
                .rename_folder(source_id, to_folder_id, &target_name)
                .await?;
        } else {
            self.api()
                .await?
                .rename_file(source_id, to_folder_id, &target_name)
                .await?;
        }

        Ok(self.next_ticket(OperationKind::Move))
    }

    async fn delete(
        &self,
        object: &ObjectRef,
        mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError> {
        match mode {
            DeleteMode::Trash => {
                return Err(BackendError::Unsupported {
                    operation: "trash on pCloud",
                });
            }
            DeleteMode::Permanent => {
                let (is_folder, id) = Self::parse_id(object)?;
                if is_folder {
                    self.api().await?.delete_folder_recursive(id).await?;
                } else {
                    self.api().await?.delete_file(id).await?;
                }
            }
        }
        Ok(self.next_ticket(OperationKind::Delete))
    }

    async fn get_thumbnail(
        &self,
        object: &ObjectRef,
        hint: ThumbnailHint,
    ) -> Result<ThumbnailResult, BackendError> {
        let (_, file_id) = Self::parse_id(object)?;
        let dim = hint.max_dimension;
        let data = self.api().await?.get_thumb(file_id, dim, dim).await?;

        Ok(ThumbnailResult {
            data,
            format: ImageFormat::Jpeg,
            width: dim,
            height: dim,
            source: ThumbnailSource::DirectApi,
        })
    }

    async fn get_metadata(
        &self,
        object: &ObjectRef,
        level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        let mut result = self.stat(object).await?;
        result.level = level;
        Ok(result)
    }

    async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
        Ok(object.key.stable_id.clone())
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet {
            can_range_read: true,
            can_rename: true,
            can_server_side_copy: true,
            can_verify_by_checksum: true,
            can_etag: true,
            ..CapabilitySet::default()
        }
    }
}

/// Validate the download envelope before exposing any body bytes. Download URLs may
/// be signed, so neither protocol errors nor transport errors may retain the URL.
async fn download_read(
    client: reqwest::Client,
    url: &str,
    range: Option<ByteRange>,
) -> Result<ReadHandle, BackendError> {
    let mut request = client.get(url);
    let requested_end = match range.as_ref() {
        Some(ByteRange {
            offset,
            length: Some(length),
        }) => Some(
            length
                .checked_sub(1)
                .and_then(|length| offset.checked_add(length))
                .ok_or_else(|| download_error("invalid requested byte range"))?,
        ),
        _ => None,
    };
    if let Some(range) = &range {
        let end = requested_end.map(|end| end.to_string()).unwrap_or_default();
        request = request.header(
            reqwest::header::RANGE,
            format!("bytes={}-{end}", range.offset),
        );
    }

    let response = request.send().await.map_err(|error| {
        let is_timeout = error.is_timeout();
        let error = error.without_url();
        if is_timeout {
            BackendError::Network(NetworkErrorDetail {
                message: format!("pCloud download request failed: {error}"),
                is_timeout: true,
            })
        } else {
            BackendError::Internal(Box::new(error))
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(BackendError::Internal(Box::new(io::Error::other(format!(
            "pCloud download failed with HTTP status {}",
            status.as_u16()
        )))));
    }

    let content_range = if let Some(range) = range {
        if status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(download_error("range download requires HTTP status 206"));
        }
        let mut headers = response
            .headers()
            .get_all(reqwest::header::CONTENT_RANGE)
            .iter();
        let parsed = headers
            .next()
            .and_then(|header| header.to_str().ok())
            .and_then(parse_content_range)
            .filter(|_| headers.next().is_none())
            .ok_or_else(|| download_error("invalid or missing Content-Range"))?;
        let (start, end, total) = parsed;
        // Open-ended requests must reach a known EOF; an unknown total cannot
        // establish that the response fulfilled the request.
        // A bounded request may extend past EOF, but may not stop short of
        // its requested end unless the known total establishes that EOF.
        let eof = total.and_then(|total| total.checked_sub(1));
        let expected_end = requested_end
            .map(|end| eof.map_or(end, |eof| end.min(eof)))
            .or(eof);
        if start != range.offset || Some(end) != expected_end {
            return Err(download_error(
                "Content-Range does not match requested range",
            ));
        }
        let span = end - start + 1; // Checked by parse_content_range.
        if let Some(length) = response.headers().get(reqwest::header::CONTENT_LENGTH) {
            let length = length.to_str().ok().and_then(parse_decimal);
            if length != Some(span) {
                return Err(download_error(
                    "Content-Length does not match Content-Range",
                ));
            }
        }
        Some(ByteRange {
            offset: start,
            length: Some(span),
        })
    } else {
        if status != reqwest::StatusCode::OK {
            return Err(download_error("full download requires HTTP status 200"));
        }
        if response
            .headers()
            .contains_key(reqwest::header::CONTENT_RANGE)
        {
            return Err(download_error(
                "unexpected partial response for full download",
            ));
        }
        None
    };

    let remaining = content_range.as_ref().and_then(|range| range.length);
    let content_length = remaining.or_else(|| response.content_length());
    let stream = futures_util::stream::try_unfold(
        (Box::pin(response.bytes_stream()), remaining),
        |(mut stream, remaining)| async move {
            match stream.next().await {
                Some(result) => {
                    let chunk = result.map_err(|error| {
                        let kind = if error.is_timeout() {
                            io::ErrorKind::TimedOut
                        } else {
                            io::ErrorKind::Other
                        };
                        io::Error::new(kind, error.without_url())
                    })?;
                    let remaining = remaining
                        .map(|remaining| {
                            remaining.checked_sub(chunk.len() as u64).ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "response body exceeds Content-Range",
                                )
                            })
                        })
                        .transpose()?;
                    Ok(Some((chunk, (stream, remaining))))
                }
                None if remaining.is_some_and(|remaining| remaining != 0) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "response body is shorter than Content-Range",
                )),
                None => Ok(None),
            }
        },
    );
    Ok(ReadHandle {
        stream: Box::pin(tokio_util::io::StreamReader::new(Box::pin(stream))),
        content_length,
        content_range,
    })
}

fn download_error(message: &'static str) -> BackendError {
    BackendError::Internal(Box::new(io::Error::new(
        io::ErrorKind::InvalidData,
        message,
    )))
}

fn parse_decimal(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// Single byte range only; multipart and unsatisfied ranges are not downloads.
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end) = (parse_decimal(start)?, parse_decimal(end)?);
    let total = if total == "*" {
        None
    } else {
        Some(parse_decimal(total)?)
    };
    end.checked_sub(start)?.checked_add(1)?;
    if total.is_some_and(|total| end >= total) {
        return None;
    }
    Some((start, end, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::{sleep, timeout, Duration};

    async fn read_request_headers(stream: &mut tokio::net::TcpStream) {
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await.expect("read HTTP request"));
            assert!(headers.len() <= 8192, "bounded HTTP request headers");
        }
    }

    #[test]
    fn stored_credential_adapter_does_not_read_credentials_during_construction() {
        let adapter = PCloudAdapter::new_with_stored_credentials(
            BackendId("pcloud:test".into()),
            "account".into(),
            "https://api.pcloud.com".into(),
        )
        .expect("approved origin");

        assert!(adapter.api.get().is_none());
    }

    #[test]
    fn stored_credential_adapter_rejects_invalid_transfer_idle_timeouts() {
        assert!(
            PCloudAdapter::new_with_stored_credentials_and_transfer_idle_timeout(
                BackendId("pcloud:test".into()),
                "account".into(),
                "https://api.pcloud.com".into(),
                Duration::ZERO,
            )
            .is_err()
        );
    }

    #[test]
    fn stored_credential_adapter_rejects_untrusted_origins_before_loading_tokens() {
        let error = match PCloudAdapter::new_with_stored_credentials(
            BackendId("pcloud:test".into()),
            "account".into(),
            "https://attacker.test".into(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("untrusted origin must be rejected"),
        };

        assert_eq!(
            error,
            "pCloud API base must be the HTTPS US or Europe API origin"
        );
    }

    #[tokio::test]
    async fn progressing_download_can_outlive_the_api_request_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept download");
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n")
                .await
                .expect("write response headers");
            for chunk in [b"ab".as_slice(), b"cd", b"ef"] {
                sleep(Duration::from_millis(45)).await;
                stream.write_all(chunk).await.expect("write response chunk");
            }
        });
        let client = PCloudApiClient::with_test_base_url_and_timeouts(
            "synthetic-token".into(),
            format!("http://{address}"),
            Duration::from_millis(100),
            Duration::from_millis(250),
        )
        .transfer_http_client();

        let started = tokio::time::Instant::now();
        let mut handle = download_read(client, &format!("http://{address}/download"), None)
            .await
            .expect("download stream");
        let mut bytes = Vec::new();
        timeout(
            Duration::from_secs(2),
            handle.stream.read_to_end(&mut bytes),
        )
        .await
        .expect("bounded download")
        .expect("read complete download");
        assert!(started.elapsed() > Duration::from_millis(100));
        assert_eq!(bytes, b"abcdef");
        server.await.expect("mock server task");
    }

    #[tokio::test]
    async fn stalled_download_expires_at_the_transfer_idle_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept stalled download");
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nx")
                .await
                .expect("write partial response");
            let mut byte = [0; 1];
            assert_eq!(
                stream
                    .read(&mut byte)
                    .await
                    .expect("observe client cancellation"),
                0,
                "dropping the stalled response stream closes its HTTP connection"
            );
        });
        let client = PCloudApiClient::with_test_base_url_and_timeouts(
            "synthetic-token".into(),
            format!("http://{address}"),
            Duration::from_millis(50),
            Duration::from_millis(60),
        )
        .transfer_http_client();
        let mut handle = download_read(client, &format!("http://{address}/stalled"), None)
            .await
            .expect("download stream");
        let mut bytes = Vec::new();
        let error = timeout(
            Duration::from_secs(1),
            handle.stream.read_to_end(&mut bytes),
        )
        .await
        .expect("bounded stalled download")
        .expect_err("stalled body must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(bytes, b"x");
        drop(handle);
        timeout(Duration::from_secs(1), server)
            .await
            .expect("server observes response cancellation")
            .expect("mock server task");
    }

    #[tokio::test]
    async fn list_names_fetches_complete_folder_only_once() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0u8; 4096];
            let request_len = stream.read(&mut request).await.expect("read request");
            assert!(String::from_utf8_lossy(&request[..request_len]).contains("/listfolder?"));

            let contents = (0..250)
                .map(|index| {
                    serde_json::json!({
                        "name": format!("file-{index}"),
                        "fileid": index + 1,
                        "isfolder": false,
                        "size": index
                    })
                })
                .collect::<Vec<_>>();
            let body = serde_json::json!({
                "result": 0,
                "metadata": {
                    "name": "root",
                    "folderid": 0,
                    "isfolder": true,
                    "contents": contents
                }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let backend_id = BackendId("pcloud:test".into());
        let adapter = PCloudAdapter::new_with_test_base(
            "token".into(),
            backend_id.clone(),
            format!("http://{address}"),
        );
        let directory = ObjectRef {
            key: ObjectKey {
                backend_id,
                stable_id: StableId("folder:0".into()),
            },
            parent_stable_id: None,
            display_path: String::new(),
        };

        let first = adapter
            .list_names(&directory, None)
            .await
            .expect("initial folder listing");
        assert_eq!(first.entries.len(), 250);
        assert!(first.next_cursor.is_none());

        server.await.expect("mock server task");
    }
}

#[cfg(test)]
#[path = "overwrite_tests.rs"]
mod overwrite_tests;

#[cfg(test)]
#[path = "read_tests.rs"]
mod read_tests;
