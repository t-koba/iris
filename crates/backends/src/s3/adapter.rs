use crate::s3::capabilities::s3_capabilities;
use async_trait::async_trait;
use aws_sdk_s3::Client as S3Client;

mod client_builder;
mod multipart;
#[cfg(test)]
mod tests;

use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::{BackendError, NetworkErrorDetail};
use iris_core_domain::types::list::{
    EntryNameOnly, EntryWithMeta, ListCursor, ListPage, ListedEntry,
};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::multipart::{MultipartPart, MultipartUploadSession};
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use multipart::{MULTIPART_CHUNK_BYTES, SINGLE_PART_UPLOAD_MAX_BYTES};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncRead;
use tokio::sync::OnceCell;

const LIST_OBJECTS_PAGE_SIZE: i32 = 1_000;

#[derive(Clone, Debug)]
pub struct S3Config {
    pub bucket: String,
    pub prefix: Option<String>,
    pub endpoint_url: Option<String>,
    pub region: String,
}

/// S3-compatible adapter: range GETs for partial reads, server-side copy within a bucket,
/// and move as copy + verify + delete.
pub struct S3Adapter {
    config: S3Config,
    backend_id: BackendId,
    client: OnceCell<S3Client>,
    ticket_counter: AtomicU64,
}

impl S3Adapter {
    async fn client(&self) -> Result<&S3Client, BackendError> {
        self.client
            .get_or_try_init(|| client_builder::build_client(&self.config))
            .await
    }

    fn object_key(&self, object: &ObjectRef) -> String {
        match &self.config.prefix {
            Some(prefix) => format!("{}/{}", prefix.trim_end_matches('/'), object.display_path),
            None => object.display_path.clone(),
        }
    }

    fn display_path_from_key(&self, key: &str) -> Result<String, BackendError> {
        let prefix = self.config.prefix.as_deref().unwrap_or("");
        let stripped = key.strip_prefix(prefix).ok_or_else(|| {
            BackendError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "S3 key '{}' does not start with configured prefix '{}'",
                    key, prefix
                ),
            )))
        })?;
        Ok(stripped.trim_start_matches('/').to_string())
    }

    fn next_ticket(&self, kind: OperationKind) -> OperationTicket {
        let id = self.ticket_counter.fetch_add(1, Ordering::Relaxed);
        OperationTicket {
            id: TicketId(id),
            kind,
        }
    }

    async fn object_exists(&self, key: &str) -> Result<bool, BackendError> {
        let res = self
            .client()
            .await?
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await;

        match res {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_not_found()) =>
            {
                Ok(false)
            }
            Err(error) => Err(self.network_error("head_object", error)),
        }
    }

    async fn resolve_write_key(
        &self,
        requested_key: String,
        on_conflict: &ConflictResolution,
    ) -> Result<String, BackendError> {
        match on_conflict {
            // Overwrite does not depend on an existence check.
            ConflictResolution::Overwrite => Ok(requested_key),
            _ if !self.object_exists(&requested_key).await? => Ok(requested_key),
            ConflictResolution::Rename => {
                let (base, ext) = split_key_extension(&requested_key);
                let mut counter = 1u32;
                loop {
                    let candidate = format!("{} ({}){}", base, counter, ext);
                    if !self.object_exists(&candidate).await? {
                        return Ok(candidate);
                    }
                    counter += 1;
                }
            }
            ConflictResolution::Fail | ConflictResolution::Skip => Err(BackendError::Conflict {
                object: ObjectRef {
                    key: ObjectKey {
                        backend_id: self.backend_id.clone(),
                        stable_id: StableId("unknown".to_string()),
                    },
                    parent_stable_id: None,
                    display_path: requested_key,
                },
            }),
        }
    }

    fn network_error(&self, operation: &str, error: impl std::fmt::Display) -> BackendError {
        BackendError::Network(NetworkErrorDetail {
            message: format!("S3 {operation} failed: {error}"),
            is_timeout: false,
        })
    }
}

#[async_trait]
impl BackendAdapter for S3Adapter {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        // Check that the bucket exists.
        self.client()
            .await?
            .head_bucket()
            .bucket(&self.config.bucket)
            .send()
            .await
            .map_err(|e| {
                BackendError::Network(NetworkErrorDetail {
                    message: format!("S3 bucket access failed: {}", e),
                    is_timeout: false,
                })
            })?;

        Ok(BackendSession {
            backend_id: self.backend_id.clone(),
            display_name: format!("S3: s3://{}", self.config.bucket),
        })
    }

    async fn list_names(
        &self,
        dir: &ObjectRef,
        cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        let prefix = if dir.display_path.is_empty() {
            self.config.prefix.clone().unwrap_or_default()
        } else {
            let key = self.object_key(dir);
            if key.ends_with('/') {
                key
            } else {
                format!("{}/", key)
            }
        };

        let mut request = self
            .client()
            .await?
            .list_objects_v2()
            .bucket(&self.config.bucket)
            .prefix(&prefix)
            .delimiter("/")
            .max_keys(LIST_OBJECTS_PAGE_SIZE);

        if let Some(c) = cursor {
            request = request.continuation_token(c.0);
        }

        let response = request.send().await.map_err(|e| {
            BackendError::Network(NetworkErrorDetail {
                message: format!("S3 list failed: {}", e),
                is_timeout: false,
            })
        })?;

        let mut entries = Vec::new();

        for cp in response.common_prefixes() {
            if let Some(p) = cp.prefix() {
                let name = p
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(p)
                    .to_string();
                let display_path = self.display_path_from_key(p)?;

                entries.push(ListedEntry::WithMeta(EntryWithMeta {
                    name: EntryNameOnly {
                        object_ref: ObjectRef {
                            key: ObjectKey {
                                backend_id: self.backend_id.clone(),
                                stable_id: StableId(p.to_string()),
                            },
                            parent_stable_id: Some(dir.key.stable_id.clone()),
                            display_path,
                        },
                        name,
                        is_dir: true,
                        is_symlink: false,
                        is_inaccessible: false,
                    },
                    size: None,
                    mtime: None,
                    ctime: None,
                }));
            }
        }

        for obj in response.contents() {
            if let Some(key) = obj.key() {
                // Skip the prefix itself.
                if key == prefix {
                    continue;
                }

                let name = key.rsplit('/').next().unwrap_or(key).to_string();
                let display_path = self.display_path_from_key(key)?;

                let etag = obj.e_tag().unwrap_or("").to_string();
                let stable_id = if etag.is_empty() {
                    StableId(key.to_string())
                } else {
                    StableId(etag.clone())
                };

                entries.push(ListedEntry::WithMeta(EntryWithMeta {
                    name: EntryNameOnly {
                        object_ref: ObjectRef {
                            key: ObjectKey {
                                backend_id: self.backend_id.clone(),
                                stable_id,
                            },
                            parent_stable_id: Some(dir.key.stable_id.clone()),
                            display_path,
                        },
                        name,
                        is_dir: false,
                        is_symlink: false,
                        is_inaccessible: false,
                    },
                    size: obj.size().map(|s| s as u64),
                    mtime: obj.last_modified().and_then(|t| {
                        let secs = t.secs();
                        let nanos = t.subsec_nanos();
                        std::time::SystemTime::UNIX_EPOCH
                            .checked_add(std::time::Duration::new(secs as u64, nanos))
                    }),
                    ctime: None,
                }));
            }
        }

        let next_cursor = response
            .next_continuation_token()
            .map(|t| ListCursor(t.to_string()));

        Ok(ListPage {
            entries,
            next_cursor,
        })
    }

    async fn stat(&self, object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        let key = self.object_key(object);

        let response = self
            .client()
            .await?
            .head_object()
            .bucket(&self.config.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| {
                BackendError::Network(NetworkErrorDetail {
                    message: format!("S3 head failed: {}", e),
                    is_timeout: false,
                })
            })?;

        let mtime = response.last_modified().and_then(|t| {
            let secs = t.secs();
            let nanos = t.subsec_nanos();
            std::time::SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::new(secs as u64, nanos))
        });

        Ok(MetadataResult {
            level: MetadataLevel::L1,
            is_dir: false,
            size: response.content_length().map(|l| l as u64),
            mtime,
            ctime: None,
            content_type: response.content_type().map(|s| s.to_string()),
            etag: response.e_tag().map(|s| s.to_string()),
        })
    }

    async fn read(
        &self,
        object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        let key = self.object_key(object);

        let mut request = self
            .client()
            .await?
            .get_object()
            .bucket(&self.config.bucket)
            .key(&key);

        if let Some(ref r) = range {
            let range_str = match r.length {
                Some(len) => format!("bytes={}-{}", r.offset, r.offset + len - 1),
                None => format!("bytes={}-", r.offset),
            };
            request = request.range(range_str);
        }

        let response = request.send().await.map_err(|e| {
            BackendError::Network(NetworkErrorDetail {
                message: format!("S3 get failed: {}", e),
                is_timeout: false,
            })
        })?;

        let content_length = response.content_length().map(|l| l as u64);
        let body = response.body.into_async_read();

        Ok(ReadHandle {
            stream: Box::pin(body),
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
        let requested_key = self.object_key(dst);
        let final_key = self.resolve_write_key(requested_key, &on_conflict).await?;
        if data.len() <= SINGLE_PART_UPLOAD_MAX_BYTES {
            self.put_object_bytes(&final_key, data).await?;
        } else {
            let mut chunks = Vec::new();
            for chunk in data.chunks(MULTIPART_CHUNK_BYTES) {
                chunks.push(chunk.to_vec());
            }
            self.upload_multipart_chunks(&final_key, chunks, Box::pin(tokio::io::empty()))
                .await?;
        }

        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn write_from_reader(
        &self,
        dst: &ObjectRef,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        content_length: Option<u64>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let requested_key = self.object_key(dst);
        let final_key = self.resolve_write_key(requested_key, &on_conflict).await?;
        self.upload_from_reader(&final_key, reader, content_length)
            .await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    fn multipart_part_size(&self) -> Option<u64> {
        Some(MULTIPART_CHUNK_BYTES as u64)
    }

    async fn create_multipart_upload(
        &self,
        dst: &ObjectRef,
        on_conflict: ConflictResolution,
    ) -> Result<MultipartUploadSession, BackendError> {
        let requested_key = self.object_key(dst);
        let key = self.resolve_write_key(requested_key, &on_conflict).await?;
        let response = self
            .client()
            .await?
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|error| self.network_error("create_multipart_upload", error))?;
        let upload_id = response
            .upload_id()
            .map(str::to_string)
            .ok_or_else(|| self.network_error("create_multipart_upload", "missing upload id"))?;

        Ok(MultipartUploadSession {
            destination: ObjectRef {
                key: ObjectKey {
                    backend_id: self.backend_id.clone(),
                    stable_id: StableId(upload_id.clone()),
                },
                parent_stable_id: dst.parent_stable_id.clone(),
                display_path: key,
            },
            upload_id,
        })
    }

    async fn list_multipart_parts(
        &self,
        session: &MultipartUploadSession,
    ) -> Result<Vec<MultipartPart>, BackendError> {
        let mut marker = None;
        let mut parts = Vec::new();
        loop {
            let response = self
                .client()
                .await?
                .list_parts()
                .bucket(&self.config.bucket)
                .key(&session.destination.display_path)
                .upload_id(&session.upload_id)
                .set_part_number_marker(marker)
                .send()
                .await
                .map_err(|error| self.network_error("list_parts", error))?;
            for part in response.parts() {
                let (Some(part_number), Some(etag), Some(size)) =
                    (part.part_number(), part.e_tag(), part.size())
                else {
                    continue;
                };
                let Ok(size) = u64::try_from(size) else {
                    continue;
                };
                parts.push(MultipartPart {
                    part_number,
                    etag: etag.to_string(),
                    size,
                });
            }
            if !response.is_truncated().unwrap_or(false) {
                break;
            }
            marker = response.next_part_number_marker().map(str::to_string);
            if marker.is_none() {
                return Err(self.network_error("list_parts", "truncated response missing marker"));
            }
        }
        Ok(parts)
    }

    async fn upload_multipart_part(
        &self,
        session: &MultipartUploadSession,
        part_number: i32,
        data: Arc<[u8]>,
    ) -> Result<MultipartPart, BackendError> {
        let size = data.len() as u64;
        let response = self
            .client()
            .await?
            .upload_part()
            .bucket(&self.config.bucket)
            .key(&session.destination.display_path)
            .upload_id(&session.upload_id)
            .part_number(part_number)
            .body(aws_sdk_s3::primitives::ByteStream::from(
                bytes::Bytes::from_owner(data),
            ))
            .send()
            .await
            .map_err(|error| self.network_error("upload_part", error))?;
        let etag = response
            .e_tag()
            .map(str::to_string)
            .ok_or_else(|| self.network_error("upload_part", "missing ETag"))?;
        Ok(MultipartPart {
            part_number,
            etag,
            size,
        })
    }

    async fn complete_multipart_upload(
        &self,
        session: &MultipartUploadSession,
        parts: Vec<MultipartPart>,
    ) -> Result<OperationTicket, BackendError> {
        let completed = parts
            .into_iter()
            .map(|part| {
                aws_sdk_s3::types::CompletedPart::builder()
                    .part_number(part.part_number)
                    .e_tag(part.etag)
                    .build()
            })
            .collect();
        self.complete_multipart_upload_by_key(
            &session.destination.display_path,
            &session.upload_id,
            completed,
        )
        .await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn abort_multipart_upload(
        &self,
        session: &MultipartUploadSession,
    ) -> Result<(), BackendError> {
        self.client()
            .await?
            .abort_multipart_upload()
            .bucket(&self.config.bucket)
            .key(&session.destination.display_path)
            .upload_id(&session.upload_id)
            .send()
            .await
            .map_err(|error| self.network_error("abort_multipart_upload", error))?;
        Ok(())
    }

    async fn mkdir(&self, parent: &ObjectRef, name: &str) -> Result<ObjectRef, BackendError> {
        // Directories are empty objects whose key ends with a slash.
        let parent_key = self.object_key(parent);
        let dir_key = if parent_key.is_empty() {
            format!("{}/", name)
        } else {
            format!("{}{}/", parent_key.trim_end_matches('/'), name)
        };

        self.client()
            .await?
            .put_object()
            .bucket(&self.config.bucket)
            .key(&dir_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|e| {
                BackendError::Network(NetworkErrorDetail {
                    message: format!("S3 mkdir failed: {}", e),
                    is_timeout: false,
                })
            })?;

        let display_path = self.display_path_from_key(&dir_key)?;

        Ok(ObjectRef {
            key: ObjectKey {
                backend_id: self.backend_id.clone(),
                stable_id: StableId(dir_key),
            },
            parent_stable_id: Some(parent.key.stable_id.clone()),
            display_path,
        })
    }

    async fn copy(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: CopyOptions,
    ) -> Result<OperationTicket, BackendError> {
        let src_key = self.object_key(src);
        let requested_dst_key = self.object_key(dst);
        let dst_key = self
            .resolve_write_key(requested_dst_key, &options.on_conflict)
            .await?;

        let copy_source = format!("{}/{}", self.config.bucket, src_key);

        self.client()
            .await?
            .copy_object()
            .bucket(&self.config.bucket)
            .copy_source(&copy_source)
            .key(&dst_key)
            .send()
            .await
            .map_err(|e| {
                BackendError::Network(NetworkErrorDetail {
                    message: format!("S3 copy failed: {}", e),
                    is_timeout: false,
                })
            })?;

        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn move_object(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        _options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
        let _ = (src, dst);
        Err(BackendError::Unsupported {
            operation: "direct move on S3 without verify",
        })
    }

    async fn delete(
        &self,
        object: &ObjectRef,
        mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError> {
        match mode {
            DeleteMode::Trash => {
                return Err(BackendError::Unsupported {
                    operation: "trash on S3",
                });
            }
            DeleteMode::Permanent => {
                let key = self.object_key(object);
                self.client()
                    .await?
                    .delete_object()
                    .bucket(&self.config.bucket)
                    .key(&key)
                    .send()
                    .await
                    .map_err(|e| {
                        BackendError::Network(NetworkErrorDetail {
                            message: format!("S3 delete failed: {}", e),
                            is_timeout: false,
                        })
                    })?;
            }
        }

        Ok(self.next_ticket(OperationKind::Delete))
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
        // The ETag serves as the stable id.
        let key = self.object_key(object);
        let response = self
            .client()
            .await?
            .head_object()
            .bucket(&self.config.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| {
                BackendError::Network(NetworkErrorDetail {
                    message: format!("S3 head for stable_id failed: {}", e),
                    is_timeout: false,
                })
            })?;

        let etag = response
            .e_tag()
            .map(|s| s.to_string())
            .unwrap_or_else(|| key.clone());

        Ok(StableId(etag))
    }

    fn capabilities(&self) -> CapabilitySet {
        s3_capabilities()
    }
}

fn split_key_extension(key: &str) -> (String, String) {
    if let Some((base, ext)) = key.rsplit_once('.') {
        if !base.ends_with('/') {
            return (base.to_string(), format!(".{}", ext));
        }
    }
    (key.to_string(), String::new())
}
