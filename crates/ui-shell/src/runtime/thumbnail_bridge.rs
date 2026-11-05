use super::*;
use iris_core_domain::types::error::BackendError;
use std::fmt;
use std::io;
use tokio_util::sync::CancellationToken;

const THUMBNAIL_CACHE_MAINTENANCE_INTERVAL: u64 = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbnailGenerationError {
    Unsupported(&'static str),
    Canceled,
    Failed(String),
}

impl fmt::Display for ThumbnailGenerationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(operation) => write!(f, "unsupported operation: {}", operation),
            Self::Canceled => write!(f, "thumbnail generation canceled"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl ThumbnailGenerationError {
    fn from_backend_error(error: BackendError) -> Self {
        match error {
            BackendError::Unsupported { operation } => Self::Unsupported(operation),
            BackendError::Internal(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|io_error| io_error.kind() == io::ErrorKind::Interrupted) =>
            {
                Self::Canceled
            }
            other => Self::Failed(other.to_string()),
        }
    }
}

impl AppRuntime {
    pub fn track_thumbnail_task(
        &self,
        path: &str,
        abort_handle: AbortHandle,
        cancel_token: CancellationToken,
    ) {
        self.thumbnail_tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                path.to_string(),
                TrackedThumbnailTask {
                    abort_handle,
                    cancel_token,
                },
            );
    }

    pub fn finish_thumbnail_task(&self, path: &str) {
        self.thumbnail_tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(path);
    }

    pub fn cancel_thumbnails<I>(&self, paths: I)
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let mut handles = self
            .thumbnail_tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for path in paths {
            if let Some(task) = handles.remove(path.as_ref()) {
                task.cancel_token.cancel();
                task.abort_handle.abort();
            }
        }
    }

    pub async fn generate_thumbnail(
        &self,
        object: &ObjectRef,
        hint: &ThumbnailHint,
        content_type: Option<&str>,
    ) -> Result<ThumbnailResult, String> {
        let cancel_token = CancellationToken::new();
        self.generate_thumbnail_detailed_with_cancellation(
            object,
            hint,
            content_type,
            &cancel_token,
        )
        .await
        .map_err(|error| error.to_string())
    }

    #[tracing::instrument(
        skip(self, hint, cancel_token),
        fields(path = %object.display_path, max_dimension = hint.max_dimension)
    )]
    pub async fn generate_thumbnail_detailed_with_cancellation(
        &self,
        object: &ObjectRef,
        hint: &ThumbnailHint,
        content_type: Option<&str>,
        cancel_token: &CancellationToken,
    ) -> Result<ThumbnailResult, ThumbnailGenerationError> {
        let path = Path::new(&object.display_path);
        let resolved = self.resolve_backend(path);
        let source_metadata = self
            .load_source_metadata(object, &resolved, content_type)
            .await
            .ok();
        let encrypt = self.should_encrypt_cache(path, resolved.kind);

        let session_cache_key = if encrypt {
            source_metadata
                .as_ref()
                .map(|metadata| thumbnail_session_cache_key(object, hint.max_dimension, metadata))
        } else {
            None
        };
        if encrypt {
            if let Some(cache_key) = session_cache_key.as_deref() {
                if let Some(cached) = self
                    .load_session_thumbnail(cache_key)
                    .map_err(ThumbnailGenerationError::Failed)?
                {
                    return Ok(cached);
                }
            }
        } else if let Some(metadata) = source_metadata.as_ref() {
            if let Some(cached) = self
                .load_persistent_thumbnail(object, hint, metadata)
                .map_err(ThumbnailGenerationError::Failed)?
            {
                return Ok(cached);
            }
        }

        let generated = if resolved.kind == VolumeKind::Local && path.exists() {
            let path = path.to_path_buf();
            let hint = hint.clone();
            let content_type = content_type.map(str::to_owned);
            let cancel_token = cancel_token.clone();
            tokio::task::spawn_blocking(move || {
                ThumbnailPipeline::generate_from_path_result_with_cancellation(
                    &path,
                    &hint,
                    content_type.as_deref(),
                    &cancel_token,
                )
            })
            .await
            .map_err(|error| ThumbnailGenerationError::Failed(error.to_string()))?
            .map_err(ThumbnailGenerationError::from_backend_error)?
        } else {
            let object_ref = self
                .source_object_ref(object, &resolved)
                .map_err(ThumbnailGenerationError::Failed)?;
            ThumbnailPipeline::generate_with_cancellation(
                resolved.adapter.as_ref(),
                &object_ref,
                hint,
                content_type,
                cancel_token,
            )
            .await
            .map_err(ThumbnailGenerationError::from_backend_error)?
        };

        if let Some(cache_key) = session_cache_key {
            self.session_cache
                .put(&cache_key, &generated.data)
                .map_err(|error| ThumbnailGenerationError::Failed(error.to_string()))?;
        } else if let Some(metadata) = source_metadata.as_ref() {
            self.store_persistent_thumbnail(object, hint, &generated, metadata)
                .map_err(ThumbnailGenerationError::Failed)?;
        }

        Ok(generated)
    }

    async fn load_source_metadata(
        &self,
        object: &ObjectRef,
        resolved: &ResolvedBackend,
        content_type: Option<&str>,
    ) -> Result<CachedObject, String> {
        if resolved.kind == VolumeKind::Local {
            let metadata = tokio::fs::metadata(&object.display_path)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(CachedObject {
                backend_id: object.key.backend_id.0.clone(),
                stable_id: object.key.stable_id.0.clone(),
                display_path: object.display_path.clone(),
                size: if metadata.is_file() {
                    Some(metadata.len())
                } else {
                    None
                },
                mtime: metadata.modified().ok(),
                content_type: content_type.map(str::to_string),
                etag: None,
                cached_level: MetadataLevel::L1,
                last_accessed: SystemTime::now(),
            });
        }

        if let Some(cached) = self.cached_remote_source_metadata(object, content_type) {
            return Ok(cached);
        }

        let source = self.source_object_ref(object, resolved)?;
        let metadata = resolved
            .adapter
            .stat(&source)
            .await
            .map_err(|error| error.to_string())?;

        Ok(CachedObject {
            backend_id: object.key.backend_id.0.clone(),
            stable_id: object.key.stable_id.0.clone(),
            display_path: object.display_path.clone(),
            size: metadata.size,
            mtime: metadata.mtime,
            content_type: metadata
                .content_type
                .or_else(|| content_type.map(str::to_string)),
            etag: metadata.etag,
            cached_level: metadata.level,
            last_accessed: SystemTime::now(),
        })
    }

    fn cached_remote_source_metadata(
        &self,
        object: &ObjectRef,
        content_type: Option<&str>,
    ) -> Option<CachedObject> {
        let mut cached = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_object(&object.key.backend_id.0, &object.key.stable_id.0)
            .ok()
            .flatten()?;

        if cached
            .last_accessed
            .elapsed()
            .ok()
            .is_none_or(|age| age > REMOTE_SOURCE_METADATA_CACHE_TTL)
        {
            return None;
        }
        if cached.size.is_none() && cached.mtime.is_none() && cached.content_type.is_none() {
            return None;
        }

        if cached.content_type.is_none() {
            cached.content_type = content_type.map(str::to_string);
        }
        cached.last_accessed = SystemTime::now();
        Some(cached)
    }

    fn load_persistent_thumbnail(
        &self,
        object: &ObjectRef,
        hint: &ThumbnailHint,
        source_metadata: &CachedObject,
    ) -> Result<Option<ThumbnailResult>, String> {
        let thumbnail_entry = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_thumbnail(
                &object.key.backend_id.0,
                &object.key.stable_id.0,
                hint.max_dimension,
            )
            .map_err(|error| error.to_string())?;
        let Some(thumbnail_entry) = thumbnail_entry else {
            return Ok(None);
        };
        if !persistent_thumbnail_is_current(&thumbnail_entry, source_metadata)
            || !thumbnail_entry.cache_file_path.exists()
        {
            return Ok(None);
        }

        let bytes = std::fs::read(&thumbnail_entry.cache_file_path).map_err(|error| {
            format!(
                "failed to read cached thumbnail {}: {}",
                thumbnail_entry.cache_file_path.display(),
                error
            )
        })?;
        Ok(thumbnail_result_from_cached_bytes(
            &thumbnail_entry.cache_file_path,
            bytes,
            Some(thumbnail_entry.format),
        ))
    }

    fn load_session_thumbnail(&self, cache_key: &str) -> Result<Option<ThumbnailResult>, String> {
        match self.session_cache.read(cache_key) {
            Ok(bytes) => {
                let result = thumbnail_result_from_cached_bytes(Path::new(""), bytes, None);
                if result.is_none() {
                    let _ = self.session_cache.remove(cache_key);
                }
                Ok(result)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn store_persistent_thumbnail(
        &self,
        object: &ObjectRef,
        hint: &ThumbnailHint,
        generated: &ThumbnailResult,
        source_metadata: &CachedObject,
    ) -> Result<(), String> {
        let cache_file_path = persistent_thumbnail_path(
            &self.catalog_cache_dir,
            object,
            hint.max_dimension,
            &generated.format,
        );

        if let Some(parent) = cache_file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&cache_file_path, &generated.data).map_err(|error| {
            format!(
                "failed to write cached thumbnail {}: {}",
                cache_file_path.display(),
                error
            )
        })?;

        let thumbnail_entry = ThumbnailEntry {
            backend_id: object.key.backend_id.0.clone(),
            stable_id: object.key.stable_id.0.clone(),
            dimension: hint.max_dimension,
            format: generated.format.clone(),
            cache_file_path: cache_file_path.clone(),
            byte_size: generated.data.len() as u64,
            generated_at: SystemTime::now(),
            source_size: source_metadata.size,
            source_mtime: source_metadata.mtime,
            source_etag: source_metadata.etag.clone(),
        };

        let previous_thumbnail_path = {
            let catalog = self
                .catalog
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let previous = catalog
                .get_thumbnail(
                    &object.key.backend_id.0,
                    &object.key.stable_id.0,
                    hint.max_dimension,
                )
                .map_err(|error| error.to_string())?;
            catalog
                .upsert_object(source_metadata)
                .map_err(|error| error.to_string())?;
            catalog
                .upsert_thumbnail(&thumbnail_entry)
                .map_err(|error| error.to_string())?;
            let writes = self.thumbnail_cache_writes.fetch_add(1, Ordering::Relaxed);
            if writes == 0 || (writes + 1).is_multiple_of(THUMBNAIL_CACHE_MAINTENANCE_INTERVAL) {
                let _ = catalog.evict();
            }
            previous
                .map(|entry| entry.cache_file_path)
                .filter(|path| path != &cache_file_path)
        };

        if let Some(previous_thumbnail_path) = previous_thumbnail_path {
            let _ = std::fs::remove_file(previous_thumbnail_path);
        }

        Ok(())
    }

    fn should_encrypt_cache(&self, source_path: &Path, kind: VolumeKind) -> bool {
        match *self
            .cache_encryption_policy
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            CacheEncryptionPolicy::Never => false,
            CacheEncryptionPolicy::Always => true,
            CacheEncryptionPolicy::Auto => {
                kind == VolumeKind::Local && is_boot_volume_path(source_path)
            }
        }
    }
}

fn thumbnail_extension(format: &ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Png => "png",
        ImageFormat::WebP => "webp",
    }
}

fn thumbnail_session_cache_key(
    object: &ObjectRef,
    dimension: u32,
    source_metadata: &CachedObject,
) -> String {
    use sha2::{Digest, Sha256};

    let mut stamp = Sha256::new();
    stamp.update(source_metadata.size.unwrap_or_default().to_le_bytes());
    if let Some(mtime) = source_metadata.mtime {
        let duration = mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        stamp.update(duration.as_secs().to_le_bytes());
        stamp.update(duration.subsec_nanos().to_le_bytes());
    }
    if let Some(etag) = &source_metadata.etag {
        stamp.update(etag.as_bytes());
    }
    let stamp = stamp.finalize()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "thumb:{}:{}:{}:{}",
        object.key.backend_id.0, object.key.stable_id.0, dimension, stamp
    )
}

fn persistent_thumbnail_path(
    base_dir: &Path,
    object: &ObjectRef,
    dimension: u32,
    format: &ImageFormat,
) -> PathBuf {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(object.key.backend_id.0.as_bytes());
    hasher.update([0]);
    hasher.update(object.key.stable_id.0.as_bytes());
    hasher.update([0]);
    hasher.update(dimension.to_le_bytes());
    let digest = hasher.finalize();
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    base_dir.join(format!("{digest}.{}", thumbnail_extension(format)))
}

fn persistent_thumbnail_is_current(
    thumbnail_entry: &ThumbnailEntry,
    source_metadata: &CachedObject,
) -> bool {
    if thumbnail_entry.source_size != source_metadata.size {
        return false;
    }
    if thumbnail_entry.source_mtime != source_metadata.mtime {
        return false;
    }

    match (&thumbnail_entry.source_etag, &source_metadata.etag) {
        (Some(cached), Some(current)) => cached == current,
        (None, None) => true,
        _ => false,
    }
}

fn thumbnail_result_from_cached_bytes(
    path: &Path,
    bytes: Vec<u8>,
    format: Option<ImageFormat>,
) -> Option<ThumbnailResult> {
    if let Some(info) = probe_encoded_image_bytes(&bytes) {
        return Some(ThumbnailResult {
            data: bytes,
            format: format.unwrap_or(info.format),
            width: info.width,
            height: info.height,
            source: ThumbnailSource::FullDecode,
        });
    }

    let format = format.or_else(|| {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(domain_thumbnail_format_from_extension)
    })?;
    let decoded = image::load_from_memory(&bytes).ok()?;

    Some(ThumbnailResult {
        data: bytes,
        format,
        width: decoded.width(),
        height: decoded.height(),
        source: ThumbnailSource::FullDecode,
    })
}

fn domain_thumbnail_format_from_extension(extension: &str) -> Option<ImageFormat> {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "png" => Some(ImageFormat::Png),
        "webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn test_cached_object(size: Option<u64>, mtime: Option<SystemTime>) -> CachedObject {
        CachedObject {
            backend_id: "local".to_string(),
            stable_id: "inode-1".to_string(),
            display_path: "/tmp/photo.jpg".to_string(),
            size,
            mtime,
            content_type: Some("image/jpeg".to_string()),
            etag: Some("etag-1".to_string()),
            cached_level: MetadataLevel::L1,
            last_accessed: SystemTime::now(),
        }
    }

    fn test_thumbnail_entry(
        source_size: Option<u64>,
        source_mtime: Option<SystemTime>,
    ) -> ThumbnailEntry {
        ThumbnailEntry {
            backend_id: "local".to_string(),
            stable_id: "inode-1".to_string(),
            dimension: 256,
            format: ImageFormat::Jpeg,
            cache_file_path: PathBuf::from("/tmp/thumb.jpg"),
            byte_size: 1024,
            generated_at: SystemTime::now(),
            source_size,
            source_mtime,
            source_etag: Some("etag-1".to_string()),
        }
    }

    #[test]
    fn persistent_thumbnail_is_current_requires_matching_source_snapshot() {
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let current = test_cached_object(Some(4_096), Some(mtime));
        let entry = test_thumbnail_entry(Some(4_096), Some(mtime));

        assert!(persistent_thumbnail_is_current(&entry, &current));
    }

    #[test]
    fn persistent_thumbnail_is_current_rejects_stale_thumbnail_after_metadata_update() {
        let old_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let new_mtime = old_mtime + Duration::from_secs(60);
        let current = test_cached_object(Some(8_192), Some(new_mtime));
        let stale_entry = test_thumbnail_entry(Some(4_096), Some(old_mtime));

        assert!(!persistent_thumbnail_is_current(&stale_entry, &current));
    }

    #[test]
    fn encrypted_session_key_tracks_source_changes() {
        let object = ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("local".to_string()),
                stable_id: StableId("inode-1".to_string()),
            },
            parent_stable_id: None,
            display_path: "/tmp/photo.jpg".to_string(),
        };
        let first_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let second_mtime = first_mtime + Duration::from_secs(1);

        let original = thumbnail_session_cache_key(
            &object,
            256,
            &test_cached_object(Some(10), Some(first_mtime)),
        );
        assert_eq!(
            original,
            thumbnail_session_cache_key(
                &object,
                256,
                &test_cached_object(Some(10), Some(first_mtime))
            )
        );
        assert_ne!(
            original,
            thumbnail_session_cache_key(
                &object,
                256,
                &test_cached_object(Some(11), Some(first_mtime))
            )
        );
        assert_ne!(
            original,
            thumbnail_session_cache_key(
                &object,
                256,
                &test_cached_object(Some(10), Some(second_mtime))
            )
        );
    }
}
