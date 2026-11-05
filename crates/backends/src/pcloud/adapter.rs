use crate::pcloud::api_client::{PCloudApiClient, PCloudMetadata};
use async_trait::async_trait;
use futures_util::StreamExt;
use iris_auth::credential_manager::CredentialManager;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::BackendError;
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

    pub fn new_with_base(access_token: String, backend_id: BackendId, api_base: String) -> Self {
        Self {
            api: OnceCell::from(PCloudApiClient::with_base_url(access_token, api_base)),
            backend_id,
            ticket_counter: AtomicU64::new(1),
            stored_credentials: None,
        }
    }

    pub fn new_with_stored_credentials(
        backend_id: BackendId,
        account_id: String,
        api_base: String,
    ) -> Self {
        Self {
            api: OnceCell::new(),
            backend_id,
            ticket_counter: AtomicU64::new(1),
            stored_credentials: Some(PCloudStoredCredentials {
                account_id,
                api_base,
            }),
        }
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
                Ok(PCloudApiClient::with_base_url(
                    token.access_token,
                    credentials.api_base.clone(),
                ))
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
            ConflictResolution::Overwrite => {
                if let Some(existing) = existing_by_name.get(&requested_name) {
                    if let Some(file_id) = existing.file_id {
                        self.api().await?.delete_file(file_id).await?;
                    } else if let Some(folder_id) = existing.folder_id {
                        self.api().await?.delete_folder_recursive(folder_id).await?;
                    }
                }
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
            ConflictResolution::Skip | ConflictResolution::Fail => {
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

        let client = self.api().await?.http_client();
        let mut request = client.get(&download_url);

        if let Some(ref r) = range {
            let range_str = match r.length {
                Some(len) => format!("bytes={}-{}", r.offset, r.offset + len - 1),
                None => format!("bytes={}-", r.offset),
            };
            request = request.header("Range", range_str);
        }

        let response = request
            .send()
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;

        let content_length = response.content_length();
        let stream = response.bytes_stream();

        use tokio_util::io::StreamReader;
        let reader = StreamReader::new(stream.map(|result| result.map_err(std::io::Error::other)));

        Ok(ReadHandle {
            stream: Box::pin(reader),
            content_length,
            content_range: range,
        })
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
        let (is_folder, source_id) = Self::parse_id(src)?;
        let (to_folder_id, target_name) =
            self.resolve_write_target(dst, &options.on_conflict).await?;
        let copied = if is_folder {
            self.api()
                .await?
                .copy_folder(source_id, to_folder_id)
                .await?
        } else {
            self.api().await?.copy_file(source_id, to_folder_id).await?
        };
        if copied.name != target_name {
            if is_folder {
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
            } else {
                self.api()
                    .await?
                    .rename_file(
                        copied.file_id.ok_or_else(|| {
                            BackendError::Internal(Box::new(std::io::Error::other(
                                "missing file_id in copied pCloud file",
                            )))
                        })?,
                        to_folder_id,
                        &target_name,
                    )
                    .await?;
            }
        }
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn move_object(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
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
            can_range_write: false,
            can_direct_thumbnail: false,
            can_rename: true,
            can_clone: false,
            can_server_side_copy: true,
            can_provide_progress: false,
            can_verify_by_checksum: true,
            can_trash: false,
            can_etag: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn stored_credential_adapter_does_not_read_credentials_during_construction() {
        let adapter = PCloudAdapter::new_with_stored_credentials(
            BackendId("pcloud:test".into()),
            "account".into(),
            "https://api.pcloud.com".into(),
        );

        assert!(adapter.api.get().is_none());
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
        let adapter = PCloudAdapter::new_with_base(
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
