use super::model::*;
use crate::remote_transport::RemoteTransportMapping;
use crate::state::{ListColumnKind, ListColumnSpec};
use iris_platform_integration::credential_store::OsCredentialStore;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

const SETTINGS_FILE_NAME: &str = "settings.json";
const LAYOUT_FILE_NAME: &str = "layout.json";
const CONNECTIONS_FILE_NAME: &str = "connections.json";
const SSH_CREDENTIAL_SERVICE: &str = "iris-ssh";

fn config_dir() -> Option<PathBuf> {
    dirs_next::config_dir().map(|d| d.join("iris"))
}

pub fn config_dir_for_storage() -> Option<PathBuf> {
    config_dir()
}

fn file_path(file_name: &str) -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(file_name))
}

/// Returns defaults when the files do not exist.
#[tracing::instrument(name = "config.load", skip_all)]
pub async fn load_config() -> LayoutConfig {
    let mut quarantined_files = Vec::new();
    let settings = load_segment::<SettingsFile>(SETTINGS_FILE_NAME, &mut quarantined_files).await;
    let layout = load_segment::<LayoutFile>(LAYOUT_FILE_NAME, &mut quarantined_files).await;
    let connections =
        load_segment::<ConnectionsFile>(CONNECTIONS_FILE_NAME, &mut quarantined_files).await;
    let notification_timeout_secs = settings.normalized_notification_timeout_secs();
    let (remote_transport_mappings, remote_transport_secret_warnings) =
        load_remote_transport_mappings(connections.remote_transport_mappings).await;

    LayoutConfig {
        show_hidden_files: settings.show_hidden_files,
        cache_encryption_policy: settings.cache_encryption_policy,
        thumbnail_cache_limit_bytes: settings.thumbnail_cache_limit_bytes,
        memory_budget_limit_bytes: settings.memory_budget_limit_bytes,
        map_tile_fetch_enabled: settings.map_tile_fetch_enabled,
        theme: settings.theme,
        locale: settings.locale,
        shortcut_overrides: settings.shortcut_overrides,
        remote_copy_undo_enabled: settings.remote_copy_undo_enabled,
        notification_timeout_secs,
        sidebar_width: layout.sidebar_width,
        right_panel_width: layout.right_panel_width,
        preview_panel_height: layout.preview_panel_height,
        window_width: layout.window_width.clamp(640.0, 7680.0),
        window_height: layout.window_height.clamp(480.0, 4320.0),
        window_x: layout.window_x.filter(|value| value.is_finite()),
        window_y: layout.window_y.filter(|value| value.is_finite()),
        preview_position: layout.preview_position,
        dual_pane_enabled: layout.dual_pane_enabled,
        pane_split_ratio: layout.pane_split_ratio,
        sidebar_visible: layout.sidebar_visible,
        preview_panel_visible: layout.preview_panel_visible,
        properties_panel_visible: layout.properties_panel_visible,
        list_columns: normalize_list_columns(layout.list_columns),
        secondary_list_columns: normalize_list_columns(layout.secondary_list_columns),
        properties_split_ratio: layout.properties_split_ratio.clamp(0.1, 0.9),
        properties_position: layout.properties_position,
        sidebar_groups: layout.sidebar_groups,
        tabs: layout.tabs,
        remote_transport_mappings,
        remote_transport_secret_warnings,
        quarantined_files,
    }
}

pub(super) fn normalize_list_columns(columns: Vec<ListColumnSpec>) -> Vec<ListColumnSpec> {
    let mut normalized = Vec::new();
    for mut column in columns {
        if normalized
            .iter()
            .any(|current: &ListColumnSpec| current.kind == column.kind)
        {
            continue;
        }
        column.width = normalize_list_column_width(column.kind, column.width);
        if column.kind == ListColumnKind::Name {
            column.visible = true;
        }
        normalized.push(column);
    }

    for mut default_column in ListColumnSpec::default_columns() {
        if normalized
            .iter()
            .any(|current| current.kind == default_column.kind)
        {
            continue;
        }
        default_column.width =
            normalize_list_column_width(default_column.kind, default_column.width);
        normalized.push(default_column);
    }

    normalized
}

fn normalize_list_column_width(kind: ListColumnKind, width: f32) -> f32 {
    match kind {
        ListColumnKind::Name => width.max(0.0),
        ListColumnKind::Size => width.clamp(50.0, 300.0),
        ListColumnKind::Date => width.clamp(60.0, 300.0),
    }
}

async fn load_remote_transport_mappings(
    mappings: Vec<RemoteTransportMappingFile>,
) -> (
    Vec<RemoteTransportMapping>,
    Vec<RemoteTransportSecretWarning>,
) {
    let mut loaded = Vec::new();
    let mut warnings = Vec::new();

    for file_mapping in mappings {
        let secret_ref = file_mapping.ssh_secret_ref.clone();
        let mut mapping = match file_mapping.into_mapping() {
            Ok(mapping) => mapping,
            Err(error) => {
                tracing::warn!("invalid remote transport mapping: {error}");
                continue;
            }
        };
        if mapping.local_mount_path.is_empty() && mapping.host.is_empty() {
            continue;
        }

        if let Some(secret_ref) = secret_ref.as_deref() {
            let (updated, warning) = apply_ssh_secret_load_result(
                mapping,
                load_ssh_secret(secret_ref.to_string()).await,
            );
            mapping = updated;
            if let Some(warning) = warning {
                tracing::warn!(
                    mapping_id_hash = %crate::observability::stable_hash(mapping.id.as_bytes()),
                    host_hash = %crate::observability::stable_hash(mapping.host.as_bytes()),
                    "failed to load SSH secret: {}",
                    warning.error
                );
                warnings.push(warning);
            }
        }

        loaded.push(mapping);
    }

    (loaded, warnings)
}

pub(crate) fn apply_ssh_secret_load_result(
    mut mapping: RemoteTransportMapping,
    result: Result<String, String>,
) -> (RemoteTransportMapping, Option<RemoteTransportSecretWarning>) {
    match result {
        Ok(secret) => {
            mapping.ssh_password = secret;
            (mapping, None)
        }
        Err(error) => {
            mapping.enabled = false;
            let warning = RemoteTransportSecretWarning {
                mapping_id: mapping.id.clone(),
                host: mapping.host.clone(),
                error,
            };
            (mapping, Some(warning))
        }
    }
}

/// Reads one config file; a file that cannot be used is set aside, named in `quarantined`, and
/// replaced by defaults.
async fn load_segment<T>(file_name: &str, quarantined: &mut Vec<String>) -> T
where
    T: Default + for<'de> Deserialize<'de> + Serialize,
{
    let Some(path) = file_path(file_name) else {
        tracing::warn!(
            config_file = file_name,
            "configuration directory is unavailable"
        );
        return T::default();
    };
    let path_hash = crate::observability::path_hash(&path);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            tracing::debug!(config_file = file_name, path_hash, "read config file");
            content
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            tracing::debug!(config_file = file_name, path_hash, "config file missing");
            return T::default();
        }
        Err(error) => {
            tracing::warn!(
                config_file = file_name,
                path_hash,
                "failed to read config file: {error}"
            );
            return T::default();
        }
    };

    match parse_segment(&content) {
        Ok(config) => config,
        Err(reason) => {
            quarantine_config_file(&path, &reason).await;
            quarantined.push(file_name.to_string());
            T::default()
        }
    }
}

/// Parses one config file; the error is the quarantine reason.
fn parse_segment<T: for<'de> Deserialize<'de>>(content: &str) -> Result<T, String> {
    let value = serde_json::from_str::<serde_json::Value>(content)
        .map_err(|error| format!("invalid-json-{error}"))?;
    schema_version_quarantine_reason(&value)?;
    serde_json::from_value(value).map_err(|error| format!("invalid-shape-{error}"))
}

/// Parses `content` as each config file kind; the entry point of the `config_load` fuzz target.
pub fn parse_config_files(content: &str) {
    let _ = parse_segment::<SettingsFile>(content);
    let _ = parse_segment::<LayoutFile>(content);
    let _ = parse_segment::<ConnectionsFile>(content);
}

pub(super) fn schema_version_quarantine_reason(value: &serde_json::Value) -> Result<(), String> {
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(version) if version == u64::from(SCHEMA_VERSION) => Ok(()),
        Some(version) => Err(format!("unsupported-schema-{version}")),
        None => Err("missing-schema-version".to_string()),
    }
}

async fn quarantine_config_file(path: &Path, reason: &str) {
    let path_hash = crate::observability::path_hash(path);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let corrupt_path = path.with_file_name(format!(
        "{}.corrupt-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.json"),
        ts
    ));
    match tokio::fs::rename(path, &corrupt_path).await {
        Ok(()) => {
            cleanup_corrupt_siblings(path).await;
            tracing::warn!(
                path_hash,
                corrupt_path_hash = crate::observability::path_hash(&corrupt_path),
                reason,
                "quarantined unreadable config file"
            );
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                path_hash,
                corrupt_path_hash = crate::observability::path_hash(&corrupt_path),
                reason,
                "failed to quarantine unreadable config file: {error}"
            );
        }
    }
}

async fn cleanup_corrupt_siblings(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let Some(base_name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let prefix = format!("{base_name}.corrupt-");
    let Ok(mut dir) = tokio::fs::read_dir(parent).await else {
        return;
    };

    let mut corrupt_files = Vec::new();
    while let Ok(Some(entry)) = dir.next_entry().await {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        let metadata_sort_key = async {
            let metadata = entry.metadata().await.ok()?;
            let modified = metadata.modified().ok()?;
            let duration = modified.duration_since(UNIX_EPOCH).ok()?;
            Some(duration.as_secs())
        }
        .await;
        let sort_key = corrupt_sort_key(&file_name).or(metadata_sort_key);
        corrupt_files.push((sort_key.unwrap_or(0), path));
    }

    corrupt_files.sort_by(|lhs, rhs| rhs.0.cmp(&lhs.0).then_with(|| rhs.1.cmp(&lhs.1)));
    for (_, path) in corrupt_files.into_iter().skip(3) {
        let _ = tokio::fs::remove_file(path).await;
    }
}

fn corrupt_sort_key(file_name: &str) -> Option<u64> {
    file_name.rsplit_once(".corrupt-")?.1.parse().ok()
}

#[tracing::instrument(name = "config.save", skip_all)]
pub async fn save_config(config: LayoutConfig) -> Result<(), String> {
    let Some(dir) = config_dir() else {
        return Err("configuration directory is unavailable".to_string());
    };
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|error| error.to_string())?;

    let settings = SettingsFile {
        schema_version: SCHEMA_VERSION,
        show_hidden_files: config.show_hidden_files,
        cache_encryption_policy: config.cache_encryption_policy,
        thumbnail_cache_limit_bytes: config.thumbnail_cache_limit_bytes,
        memory_budget_limit_bytes: config.memory_budget_limit_bytes,
        map_tile_fetch_enabled: config.map_tile_fetch_enabled,
        theme: config.theme,
        locale: config.locale,
        shortcut_overrides: config.shortcut_overrides,
        remote_copy_undo_enabled: config.remote_copy_undo_enabled,
        notification_timeout_secs: config.notification_timeout_secs,
    };
    let layout = LayoutFile {
        schema_version: SCHEMA_VERSION,
        sidebar_width: config.sidebar_width,
        right_panel_width: config.right_panel_width,
        preview_panel_height: config.preview_panel_height,
        window_width: config.window_width,
        window_height: config.window_height,
        window_x: config.window_x,
        window_y: config.window_y,
        preview_position: config.preview_position,
        dual_pane_enabled: config.dual_pane_enabled,
        pane_split_ratio: config.pane_split_ratio,
        sidebar_visible: config.sidebar_visible,
        preview_panel_visible: config.preview_panel_visible,
        properties_panel_visible: config.properties_panel_visible,
        list_columns: config.list_columns,
        secondary_list_columns: config.secondary_list_columns,
        properties_split_ratio: config.properties_split_ratio,
        properties_position: config.properties_position,
        sidebar_groups: config.sidebar_groups,
        tabs: config.tabs,
    };
    persist_remote_transport_secrets(&config.remote_transport_mappings).await?;
    let connections = ConnectionsFile {
        schema_version: SCHEMA_VERSION,
        remote_transport_mappings: config
            .remote_transport_mappings
            .iter()
            .map(RemoteTransportMappingFile::from)
            .collect(),
    };

    atomic_write_json(&dir.join(SETTINGS_FILE_NAME), &settings).await?;
    atomic_write_json(&dir.join(LAYOUT_FILE_NAME), &layout).await?;
    atomic_write_json(&dir.join(CONNECTIONS_FILE_NAME), &connections).await?;
    Ok(())
}

async fn persist_remote_transport_secrets(
    mappings: &[RemoteTransportMapping],
) -> Result<(), String> {
    for mapping in mappings {
        if mapping.ssh_password.is_empty() {
            continue;
        }
        store_ssh_secret(mapping.id.clone(), mapping.ssh_password.clone()).await?;
    }
    Ok(())
}

pub async fn delete_remote_transport_secret(mapping_id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        OsCredentialStore
            .delete(SSH_CREDENTIAL_SERVICE, &mapping_id)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn store_ssh_secret(mapping_id: String, secret: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        OsCredentialStore
            .store(SSH_CREDENTIAL_SERVICE, &mapping_id, &secret)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn load_ssh_secret(mapping_id: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        OsCredentialStore
            .load(SSH_CREDENTIAL_SERVICE, &mapping_id)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn atomic_write_json<T>(path: &Path, value: &T) -> Result<(), String>
where
    T: Serialize,
{
    let Some(parent) = path.parent() else {
        return Err(format!("{} has no parent directory", path.display()));
    };
    let path_hash = crate::observability::path_hash(path);
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| error.to_string())?;

    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let tmp_path = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("json")
    ));
    {
        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes)
            .await
            .map_err(|error| error.to_string())?;
        file.write_all(b"\n")
            .await
            .map_err(|error| error.to_string())?;
        file.sync_all().await.map_err(|error| error.to_string())?;
    }

    rename_replacing(&tmp_path, path).await?;
    sync_parent_dir(parent).await;
    tracing::debug!(
        path_hash,
        bytes = bytes.len(),
        "atomically wrote config segment"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
async fn rename_replacing(tmp_path: &Path, path: &Path) -> Result<(), String> {
    let mut last_error = None;
    for _ in 0..5 {
        match tokio::fs::rename(tmp_path, path).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if let Err(remove_error) = tokio::fs::remove_file(path).await {
                    if remove_error.kind() != ErrorKind::NotFound {
                        return Err(remove_error.to_string());
                    }
                }
                last_error = Some(error);
            }
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Err(last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "rename failed".to_string()))
}

#[cfg(not(target_os = "windows"))]
async fn rename_replacing(tmp_path: &Path, path: &Path) -> Result<(), String> {
    tokio::fs::rename(tmp_path, path)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
async fn sync_parent_dir(parent: &Path) {
    if let Ok(dir) = tokio::fs::File::open(parent).await {
        if let Err(error) = dir.sync_all().await {
            tracing::debug!(
                path_hash = crate::observability::path_hash(parent),
                "failed to fsync config directory: {error}"
            );
        }
    }
}

#[cfg(not(unix))]
async fn sync_parent_dir(_parent: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_sort_key_parses_timestamp_suffix() {
        assert_eq!(corrupt_sort_key("settings.json.corrupt-123"), Some(123));
        assert_eq!(corrupt_sort_key("settings.json"), None);
        assert_eq!(corrupt_sort_key("settings.json.corrupt-nope"), None);
    }
}
