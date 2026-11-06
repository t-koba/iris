mod backend_lifecycle;
mod directory;
mod dnd;
mod fileops_dispatch;
mod jobs_bridge;
mod map_tiles;
mod negative_cache;
mod path_routing;
mod remote_shortcuts;
mod thumbnail_bridge;
mod transfer_driver;
mod transfer_verify;

pub(crate) use fileops_dispatch::local_list_entry;
pub use thumbnail_bridge::ThumbnailGenerationError;

use crate::input::dnd::DndOperation;
use crate::state::{CacheEncryptionPolicy, PendingFileItem};
use dnd::forced_dnd_operation;
use iris_auth::credential_manager::CredentialManager;
use iris_auth::token::TokenSet;
use iris_backends::localfs::adapter::LocalFsAdapter;
use iris_backends::localfs::stable_id::get_stable_id as get_local_stable_id;
use iris_backends::pcloud::adapter::PCloudAdapter;
use iris_backends::pcloud::api_client::PCloudApiClient;
use iris_backends::registry::BackendRegistry;
use iris_backends::s3::adapter::{
    S3Adapter, S3Config, S3_CUSTOM_ENDPOINT_CONDITIONAL_WRITES_UNCONFIRMED,
};
use iris_backends::smb::adapter::SmbAdapter;
use iris_backends::smb::connection::SmbConnectionConfig;
use iris_catalog::repository::{
    CachedDirectoryEntry, CachedObject, CatalogRepository, DirectorySnapshot, ThumbnailEntry,
};
use iris_catalog::session_cache::SessionCache;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::event::{DomainEvent, EventBus};
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::metadata::MetadataLevel;
use iris_core_domain::types::metadata::MetadataResult;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::progress::ProgressInfo;
use iris_core_domain::types::thumbnail::{
    ImageFormat, ThumbnailHint, ThumbnailResult, ThumbnailSource,
};
use iris_core_domain::types::transfer::{JobId, JobState, TaskId, TaskKind, TaskState};
use iris_jobstore::repository::{PersistedJob, PersistedTask};
use iris_jobstore::writer_task::JobStore;
use iris_path_service::{
    expand_home_path, is_boot_volume_path, join_display_path, normalize_local_path,
    normalize_relative_display_path, parent_relative_display_path, parse_pcloud_callback,
    same_filesystem, sanitize_file_name, short_config_hash, url_encode_component,
};
use iris_platform_integration::default_app;
use iris_platform_integration::mount::{
    list_mounted_volumes, MountedVolume, NetworkMountProtocol, VolumeKind,
};
use iris_platform_integration::path::unique_sibling_path;
use iris_thumb_preview::encoded::probe_encoded_image_bytes;
use iris_thumb_preview::metadata::ImageMetadata;
use iris_thumb_preview::pipeline::ThumbnailPipeline;
use iris_transfer::executor::{
    client_relay_copy, move_via_relay_with_verify_and_control, CopyRelayOptions, MoveRelayOptions,
    ResumableTransferContext, TransferControl, TransferResult,
};
use iris_transfer::job_manager::{ConcurrencyLimits, JobManager};
use iris_transfer::method_selector::{
    select_copy_method, select_move_method, CopyMethod, MoveMethod,
};
use negative_cache::{NegativeCacheKind, NegativePathCache};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration as StdDuration, SystemTime};
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct TransferItemOutcome {
    pub bytes_completed: u64,
}

#[derive(Clone, Debug)]
pub struct DirectorySnapshotWrite {
    backend_id: BackendId,
    dir_stable_id: StableId,
    generation: u64,
}

#[derive(Clone)]
struct RegisteredBackend {
    backend_id: BackendId,
    root_path: PathBuf,
    kind: VolumeKind,
    display_name: String,
    root_stable_id: StableId,
}

#[derive(Clone)]
struct ResolvedBackend {
    backend_id: BackendId,
    root_path: PathBuf,
    kind: VolumeKind,
    root_stable_id: StableId,
    adapter: Arc<dyn BackendAdapter>,
}

#[derive(Clone, Debug)]
pub struct CloudSidebarLocation {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct SmbNativeConnectRequest {
    pub display_name: String,
    pub server: String,
    pub port: u16,
    pub share_name: String,
    pub username: Option<String>,
    pub domain: Option<String>,
    pub password: Option<String>,
    pub allow_guest: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageCategory {
    Thumbnail,
    Index,
    Tile,
    Log,
    Quarantine,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
enum CloudAccountConfig {
    S3 {
        backend_id: String,
        display_name: String,
        bucket: String,
        region: String,
        endpoint_url: Option<String>,
        #[serde(default)]
        custom_endpoint_conditional_writes_confirmed: bool,
        prefix: Option<String>,
    },
    PCloud {
        backend_id: String,
        display_name: String,
        account_id: String,
        api_base: String,
    },
}

#[derive(Clone, Debug)]
pub struct ConflictMetadata {
    pub source: ObjectRef,
    pub destination_path: String,
    pub source_size: u64,
    pub destination_size: u64,
    pub source_mtime: Option<SystemTime>,
    pub destination_mtime: Option<SystemTime>,
}

#[derive(Default)]
struct ActiveJobProgress {
    committed_bytes: u64,
    task_bytes: HashMap<TaskId, u64>,
}

struct TrackedThumbnailTask {
    abort_handle: AbortHandle,
    cancel_token: CancellationToken,
}

pub struct AppRuntime {
    event_bus: Arc<EventBus>,
    registry: Mutex<BackendRegistry>,
    registered_backends: Mutex<Vec<RegisteredBackend>>,
    job_store: Arc<JobStore>,
    job_manager: Arc<JobManager>,
    active_transfers: Mutex<HashMap<u64, TransferControl>>,
    active_transfer_progress: Mutex<HashMap<u64, ActiveJobProgress>>,
    catalog: Arc<Mutex<CatalogRepository>>,
    catalog_cache_dir: PathBuf,
    open_cache_dir: PathBuf,
    index_cache_dir: PathBuf,
    tile_cache_dir: PathBuf,
    map_tile_cache_writes: AtomicU64,
    session_cache: Arc<SessionCache>,
    cache_encryption_policy: Mutex<CacheEncryptionPolicy>,
    thumbnail_tasks: Mutex<HashMap<String, TrackedThumbnailTask>>,
    thumbnail_cache_writes: AtomicU64,
    known_remote_paths: Mutex<HashMap<(String, String), StableId>>,
    negative_path_cache: Mutex<NegativePathCache>,
    /// User-configured remote transfer mappings (SMB/NFS mount -> SSH/rsync).
    remote_transport_mappings: Mutex<Vec<crate::remote_transport::RemoteTransportMapping>>,
}

static APP_RUNTIME: OnceLock<Arc<AppRuntime>> = OnceLock::new();
const REMOTE_SOURCE_METADATA_CACHE_TTL: StdDuration = StdDuration::from_secs(300);
const NEGATIVE_PATH_CACHE_TTL: StdDuration = StdDuration::from_secs(3);
const DIRECTORY_SNAPSHOT_FAST_PATH_TTL: StdDuration = StdDuration::from_secs(30);

pub fn app_runtime() -> &'static Arc<AppRuntime> {
    APP_RUNTIME.get_or_init(AppRuntime::initialize)
}

fn user_facing_backend_error(error: BackendError) -> String {
    let detail = error.to_string();
    tracing::debug!(error = %detail, "backend operation failed");
    match error {
        BackendError::Auth(_) => crate::i18n::tr("backend-error-auth"),
        BackendError::Network(detail) if detail.is_timeout => {
            crate::i18n::tr("backend-error-network-timeout")
        }
        BackendError::Network(_) => crate::i18n::tr("backend-error-network"),
        BackendError::NotFound { .. } => crate::i18n::tr("backend-error-not-found"),
        BackendError::PermissionDenied { .. } => crate::i18n::tr("backend-error-permission"),
        BackendError::StorageFull => crate::i18n::tr("backend-error-storage-full"),
        BackendError::Unsupported { operation }
            if operation == S3_CUSTOM_ENDPOINT_CONDITIONAL_WRITES_UNCONFIRMED =>
        {
            crate::i18n::tr("connections-s3-conditional-writes-required")
        }
        BackendError::Unsupported { .. } => crate::i18n::tr("backend-error-unsupported"),
        BackendError::Conflict { .. } => crate::i18n::tr("backend-error-conflict"),
        BackendError::Internal(_) => crate::i18n::tr("backend-error-internal"),
    }
}

/// Creates a local directory unless it exists; returns whether it was created.
async fn create_local_directory_if_missing(
    resolved: &ResolvedBackend,
    path: &Path,
) -> Result<bool, String> {
    if tokio::fs::metadata(path).await.is_ok() {
        return Ok(false);
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    let parent_ref = ObjectRef {
        key: ObjectKey {
            backend_id: resolved.backend_id.clone(),
            stable_id: get_local_stable_id(parent)
                .unwrap_or_else(|_| StableId(parent.to_string_lossy().to_string())),
        },
        parent_stable_id: None,
        display_path: parent
            .strip_prefix(&resolved.root_path)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .to_string(),
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no final component", path.display()))?;
    resolved
        .adapter
        .mkdir(&parent_ref, name)
        .await
        .map_err(|error| error.to_string())?;
    Ok(true)
}

async fn verify_and_delete_move(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    high_risk_path: bool,
) -> Result<(), String> {
    transfer_verify::verify_object_copy(src_backend, src, dst_backend, dst, high_risk_path).await?;
    src_backend
        .delete(src, DeleteMode::Permanent)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn wait_if_paused_or_canceled(control: &TransferControl) -> Result<(), String> {
    let mut pause_rx = control.pause_receiver();
    while *pause_rx.borrow() {
        if control.cancel_token().is_cancelled() {
            return Err("transfer canceled".to_string());
        }
        pause_rx
            .changed()
            .await
            .map_err(|_| "pause control channel closed".to_string())?;
    }

    if control.cancel_token().is_cancelled() {
        return Err("transfer canceled".to_string());
    }

    Ok(())
}

fn resolve_destination_path(
    requested_dst: &Path,
    resolution: &ConflictResolution,
) -> Result<PathBuf, String> {
    if !requested_dst.exists() {
        return Ok(requested_dst.to_path_buf());
    }

    match resolution {
        ConflictResolution::Overwrite | ConflictResolution::Skip => Ok(requested_dst.to_path_buf()),
        ConflictResolution::Rename => Ok(unique_sibling_path(requested_dst)),
        ConflictResolution::Fail => Err(format!(
            "destination already exists: {}",
            requested_dst.display()
        )),
    }
}

fn runtime_data_dir() -> PathBuf {
    dirs_next::data_local_dir()
        .or_else(dirs_next::config_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("iris")
}

#[cfg(windows)]
fn default_local_root() -> PathBuf {
    local_backend_for_path(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(r"C:\"))).1
}

#[cfg(not(windows))]
fn default_local_root() -> PathBuf {
    PathBuf::from("/")
}

#[cfg(windows)]
fn local_backend_for_path(path: &Path) -> (BackendId, PathBuf) {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return (BackendId("local".to_string()), PathBuf::from(r"C:\"));
    };

    let disk = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            format!("{}:\\", char::from(letter))
        }
        _ => String::from(r"C:\"),
    };
    let root_path = PathBuf::from(&disk);
    (BackendId(format!("local:{}", disk)), root_path)
}
enum RsyncStrategy {
    Ssh,
}

impl RsyncStrategy {
    fn remote_spec(
        &self,
        map: &crate::remote_transport::RemoteTransportMapping,
        local: &Path,
    ) -> Option<String> {
        match self {
            Self::Ssh => map.rsync_ssh_spec_for(local),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
        }
    }
}

async fn rsync_binary_available() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    if let Some(&cached) = CACHE.get() {
        return cached;
    }
    #[cfg(target_os = "windows")]
    let locator = ("where", "rsync.exe");
    #[cfg(not(target_os = "windows"))]
    let locator = ("which", "rsync");
    let available = tokio::process::Command::new(locator.0)
        .arg(locator.1)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    let _ = CACHE.set(available);
    available
}

async fn sum_transferred_bytes(items: &[PendingFileItem]) -> u64 {
    let mut total = 0u64;
    for item in items {
        let src = Path::new(&item.source.display_path);
        if let Ok(meta) = tokio::fs::metadata(src).await {
            total += meta.len();
        }
    }
    total
}

fn persisted_task_id(job_id: &JobId, task_index: usize) -> TaskId {
    TaskId(job_id.0.saturating_mul(1_000_000) + task_index as u64 + 1)
}

/// Bytes of the regular files under `path`, for progress totals. Symlinks are not followed, so a
/// link back to an ancestor cannot make the walk endless.
fn path_total_bytes(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_dir() {
        return Some(if metadata.is_file() {
            metadata.len()
        } else {
            0
        });
    }

    let mut total = 0u64;
    for entry in std::fs::read_dir(path).ok()? {
        let entry = entry.ok()?;
        total = total.saturating_add(path_total_bytes(&entry.path())?);
    }
    Some(total)
}

/// Checks a local directory copy before its source is deleted; runs on the blocking pool because
/// it walks both trees and may hash every file.
async fn verify_directory_mirror(
    src: &Path,
    dst: &Path,
    high_risk_path: bool,
) -> Result<(), String> {
    let (src, dst) = (src.to_path_buf(), dst.to_path_buf());
    tokio::task::spawn_blocking(move || verify_path_mirror(&src, &dst, high_risk_path))
        .await
        .map_err(|error| format!("verify task failed: {error}"))?
}

fn verify_path_mirror(src: &Path, dst: &Path, high_risk_path: bool) -> Result<(), String> {
    // A recreated link must point where the original pointed; its target is not part of the copy.
    let is_link = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    };
    if is_link(src) && is_link(dst) {
        let src_target = std::fs::read_link(src).map_err(|error| error.to_string())?;
        let dst_target = std::fs::read_link(dst).map_err(|error| error.to_string())?;
        if src_target != dst_target {
            return Err(format!(
                "verify failed: link target mismatch src={} dst={}",
                src.display(),
                dst.display()
            ));
        }
        return Ok(());
    }

    let src_meta = std::fs::metadata(src).map_err(|error| error.to_string())?;
    let dst_meta = std::fs::metadata(dst).map_err(|error| error.to_string())?;

    if src_meta.is_dir() != dst_meta.is_dir() {
        return Err(format!(
            "verify failed: type mismatch src={} dst={}",
            src.display(),
            dst.display()
        ));
    }

    if src_meta.is_file() {
        if src_meta.len() != dst_meta.len() {
            return Err(format!(
                "verify failed: size mismatch src={} dst={}",
                src.display(),
                dst.display()
            ));
        }

        // Equal mtimes are expected because copies carry the source's, but a destination may
        // round it (FAT keeps 2 s, exFAT 10 ms), so a difference is settled by content.
        let mtime_differs = match (src_meta.modified(), dst_meta.modified()) {
            (Ok(src_mtime), Ok(dst_mtime)) => src_mtime != dst_mtime,
            _ => false,
        };
        if (high_risk_path || mtime_differs) && hash_path(src)? != hash_path(dst)? {
            return Err(format!(
                "verify failed: content mismatch src={} dst={}",
                src.display(),
                dst.display()
            ));
        }
        return Ok(());
    }

    let src_children = read_dir_map(src)?;
    let dst_children = read_dir_map(dst)?;
    if src_children.len() != dst_children.len() {
        return Err(format!(
            "verify failed: directory child count mismatch src={} dst={}",
            src.display(),
            dst.display()
        ));
    }

    for (name, src_child) in src_children {
        let Some(dst_child) = dst_children.get(&name) else {
            return Err(format!(
                "verify failed: missing destination child '{}' under {}",
                name,
                dst.display()
            ));
        };
        verify_path_mirror(&src_child, dst_child, high_risk_path)?;
    }

    Ok(())
}

fn read_dir_map(path: &Path) -> Result<HashMap<String, PathBuf>, String> {
    let mut entries = HashMap::new();
    for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        entries.insert(
            entry.file_name().to_string_lossy().to_string(),
            entry.path(),
        );
    }
    Ok(entries)
}

fn hash_path(path: &Path) -> Result<[u8; 32], String> {
    use sha2::{Digest, Sha256};

    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn backend_type_from_id(backend_id: &BackendId) -> String {
    backend_id
        .0
        .split(':')
        .next()
        .unwrap_or(backend_id.0.as_str())
        .to_string()
}

const REMOTE_ROOT_MARKER: &str = "__iris_remote__";

fn cloud_accounts_path() -> PathBuf {
    runtime_data_dir().join("cloud-accounts.json")
}

fn virtual_root_path(backend_id: &BackendId) -> PathBuf {
    PathBuf::from("/")
        .join(REMOTE_ROOT_MARKER)
        .join(&backend_id.0)
}

fn cloud_account_backend_id(config: &CloudAccountConfig) -> &str {
    match config {
        CloudAccountConfig::S3 { backend_id, .. }
        | CloudAccountConfig::PCloud { backend_id, .. } => backend_id,
    }
}

fn is_cloud_backend_id(backend_id: &BackendId) -> bool {
    backend_id.0.starts_with("s3:") || backend_id.0.starts_with("pcloud:")
}

#[cfg(test)]
mod cloud_config_tests {
    use super::CloudAccountConfig;

    #[test]
    fn s3_confirmation_defaults_closed_and_round_trips() {
        let mut saved = serde_json::json!({
            "provider": "s3", "backend_id": "s3:test", "display_name": "Test",
            "bucket": "archive", "region": "us-east-1", "endpoint_url": "https://s3.test",
            "prefix": null
        });
        for confirmed in [false, true] {
            if confirmed {
                saved["custom_endpoint_conditional_writes_confirmed"] = true.into();
            }
            let config: CloudAccountConfig = serde_json::from_value(saved.clone()).unwrap();
            assert!(matches!(&config, CloudAccountConfig::S3 {
                custom_endpoint_conditional_writes_confirmed, ..
            } if *custom_endpoint_conditional_writes_confirmed == confirmed));
            let round_trip: CloudAccountConfig =
                serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
            assert!(matches!(round_trip, CloudAccountConfig::S3 {
                custom_endpoint_conditional_writes_confirmed, ..
            } if custom_endpoint_conditional_writes_confirmed == confirmed));
        }
        saved["custom_endpoint_conditional_writes_confirmed"] = "true".into();
        assert!(serde_json::from_value::<CloudAccountConfig>(saved).is_err());
    }
}
