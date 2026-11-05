use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::MetadataLevel;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::progress::ProgressInfo;
use iris_core_domain::types::transfer::{JobId, JobState};
use iris_remote_transport::{RemoteTransportKind, SshAuthKind};
use iris_ui_shell::state::{
    AppState, CacheEncryptionPolicy, CloudConnectFormState, DialogState, JobDisplayInfo,
    NetworkConnectFormState, Notification, NotificationAction, NotificationSeverity, PaneState,
    PreviewData, PreviewPosition, PropertiesPosition, SettingsSection, SidebarGroup,
    SidebarGroupKind, SidebarItem,
};
use serde::Serialize;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

#[derive(Serialize)]
pub struct StateSnapshot {
    active_pane: usize,
    dual_pane_enabled: bool,
    sidebar_visible: bool,
    sidebar_jobs_visible: bool,
    preview_panel_visible: bool,
    properties_panel_visible: bool,
    cache_encryption_policy: String,
    preview_position: String,
    properties_position: String,
    thumbnail_cache_limit_mib: usize,
    active_dialog: Option<DialogSnapshot>,
    clipboard: Option<String>,
    notifications: Vec<NotificationSnapshot>,
    jobs: Vec<JobSnapshot>,
    panes: Vec<PaneSnapshot>,
    sidebar_groups: Vec<SidebarGroupSnapshot>,
    remote_transport_count: usize,
    remote_transport_editing: bool,
    preview_loading: bool,
    preview_failed: bool,
    preview_summary: Option<String>,
}

#[derive(Serialize)]
pub struct ViewSnapshot {
    pub case: String,
    pub component: String,
    pub title: String,
    pub theme: String,
    pub state: StateSnapshot,
}

#[derive(Serialize)]
pub struct TransitionSnapshot {
    pub case: String,
    pub message: String,
    pub before: StateSnapshot,
    pub after: StateSnapshot,
}

#[derive(Serialize)]
pub struct DialogSnapshot {
    pub kind: String,
    pub summary: String,
}

#[derive(Serialize)]
struct NotificationSnapshot {
    severity: String,
    title: String,
    message: String,
    actions: Vec<String>,
}

#[derive(Serialize)]
struct JobSnapshot {
    id: u64,
    state: String,
    description: String,
    bytes_completed: u64,
    bytes_total: Option<u64>,
    files_completed: u64,
    files_total: Option<u64>,
}

#[derive(Serialize)]
struct PaneSnapshot {
    current_path: String,
    entry_names: Vec<String>,
    selected_names: Vec<String>,
    focused_index: Option<u32>,
    history: Vec<String>,
    history_index: usize,
    view_mode: String,
    sort_key: String,
    sort_order: String,
    error_message: Option<String>,
}

#[derive(Serialize)]
struct SidebarGroupSnapshot {
    name: String,
    collapsed: bool,
    system: bool,
    item_names: Vec<String>,
}

pub fn assert_json_snapshot(name: &str, value: &impl Serialize) {
    let snapshot = serde_json::to_string_pretty(value).expect("serialize snapshot") + "\n";
    let path = snapshot_path(name);
    let update = std::env::var("IRIS_SNAPSHOT_UPDATE")
        .map(|value| value != "0")
        .unwrap_or(false);

    if update || !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create snapshot directory");
        }
        fs::write(&path, &snapshot).expect("write snapshot");
    }

    let expected = fs::read_to_string(&path).expect("read snapshot");
    assert_eq!(expected, snapshot, "snapshot mismatch: {}", path.display());
}

pub fn snapshot_view(case: &str, component: &str, state: &AppState) {
    let appearance = if iris_ui_shell::app::theme(state).extended_palette().is_dark {
        "Dark"
    } else {
        "Light"
    };
    assert_json_snapshot(
        case,
        &ViewSnapshot {
            case: case.to_string(),
            component: component.to_string(),
            title: iris_ui_shell::app::title(state),
            theme: appearance.to_string(),
            state: snapshot_state(state),
        },
    );
}

pub fn snapshot_state(state: &AppState) -> StateSnapshot {
    StateSnapshot {
        active_pane: state.active_pane,
        dual_pane_enabled: state.dual_pane_enabled,
        sidebar_visible: state.sidebar_visible,
        sidebar_jobs_visible: state.sidebar_jobs_visible,
        preview_panel_visible: state.preview_panel_visible,
        properties_panel_visible: state.properties_panel_visible,
        cache_encryption_policy: state.cache_encryption_policy.to_string(),
        preview_position: state.preview_position.to_string(),
        properties_position: state.properties_position.to_string(),
        thumbnail_cache_limit_mib: state.thumbnail_cache_limit_bytes / (1024 * 1024),
        active_dialog: dialog_snapshot(state.active_dialog.as_ref()),
        clipboard: clipboard_snapshot(state),
        notifications: state
            .notifications
            .iter()
            .map(|notification| NotificationSnapshot {
                severity: format!("{:?}", notification.severity),
                title: notification.title.clone(),
                message: notification.message.clone(),
                actions: notification
                    .actions
                    .iter()
                    .map(|action| format!("{action:?}"))
                    .collect(),
            })
            .collect(),
        jobs: state
            .jobs
            .iter()
            .map(|job| JobSnapshot {
                id: job.id.0,
                state: format!("{:?}", job.state),
                description: job.description.clone(),
                bytes_completed: job.progress.bytes_completed,
                bytes_total: job.progress.bytes_total,
                files_completed: job.progress.files_completed,
                files_total: job.progress.files_total,
            })
            .collect(),
        panes: state
            .active_panes()
            .into_iter()
            .map(pane_snapshot)
            .collect(),
        sidebar_groups: state
            .sidebar_groups
            .iter()
            .map(|group| SidebarGroupSnapshot {
                name: group.name.clone(),
                collapsed: group.collapsed,
                system: group.is_system(),
                item_names: group.items.iter().map(|item| item.name.clone()).collect(),
            })
            .collect(),
        remote_transport_count: state.remote_transport_mappings.len(),
        remote_transport_editing: state.remote_transport_edit_buffer.is_some(),
        preview_loading: state.preview_loading,
        preview_failed: state.preview_failed,
        preview_summary: preview_summary(state.preview_data.as_ref()),
    }
}

pub fn sample_state() -> AppState {
    iris_ui_shell::i18n::initialize(Some("ja-JP"));

    let mut state = AppState::default();
    state.theme_choice = iris_ui_shell::config::ThemeChoice::Dark;
    state.sidebar_visible = true;
    state.sidebar_jobs_visible = false;
    state.preview_panel_visible = true;
    state.properties_panel_visible = false;
    state.preview_position = PreviewPosition::Right;
    state.properties_position = PropertiesPosition::BelowPreview;
    state.cache_encryption_policy = CacheEncryptionPolicy::Auto;
    state.thumbnail_cache_limit_bytes = 48 * 1024 * 1024;
    state.next_notification_id = 3;

    *state.pane_mut(0) = sample_pane(
        "/Users/test/Pictures",
        &["/Users/test", "/Users/test/Pictures"],
        vec![
            list_entry(
                "local-main",
                "/Users/test/Pictures/photo.jpg",
                false,
                Some(512_000),
            ),
            list_entry("local-main", "/Users/test/Pictures/album", true, None),
            list_entry(
                "local-main",
                "/Users/test/Pictures/clip.mp4",
                false,
                Some(2_048_000),
            ),
        ],
    );
    state.pane_mut(0).select_single(0);
    *state.pane_mut(1) = sample_pane(
        "/Users/test/Downloads",
        &["/Users/test", "/Users/test/Downloads"],
        vec![
            list_entry("local-secondary", "/Users/test/Downloads/inbox", true, None),
            list_entry(
                "local-secondary",
                "/Users/test/Downloads/reference.pdf",
                false,
                Some(3_145_728),
            ),
        ],
    );

    state.sidebar_groups = vec![
        SidebarGroup {
            name: "場所".into(),
            collapsed: false,
            kind: SidebarGroupKind::Places,
            items: vec![
                SidebarItem {
                    name: "ホーム".into(),
                    path: "/Users/test".into(),
                    badge: "~".into(),
                    badge_color: [0.2, 0.2, 0.2, 1.0],
                    ejectable: false,
                },
                SidebarItem {
                    name: "ピクチャ".into(),
                    path: "/Users/test/Pictures".into(),
                    badge: "Px".into(),
                    badge_color: [0.4, 0.6, 0.9, 1.0],
                    ejectable: false,
                },
            ],
        },
        SidebarGroup {
            name: "お気に入り".into(),
            collapsed: false,
            kind: SidebarGroupKind::Favorites,
            items: vec![
                SidebarItem {
                    name: "Projects".into(),
                    path: "/Users/test/Projects".into(),
                    badge: "*".into(),
                    badge_color: [0.8, 0.7, 0.2, 1.0],
                    ejectable: false,
                },
                SidebarItem {
                    name: "Archive".into(),
                    path: "/Users/test/Archive".into(),
                    badge: "*".into(),
                    badge_color: [0.7, 0.6, 0.2, 1.0],
                    ejectable: false,
                },
            ],
        },
        SidebarGroup {
            name: "リモート".into(),
            collapsed: false,
            kind: SidebarGroupKind::Custom,
            items: vec![SidebarItem {
                name: "NAS".into(),
                path: "/Volumes/NAS".into(),
                badge: "N".into(),
                badge_color: [0.3, 0.8, 0.8, 1.0],
                ejectable: false,
            }],
        },
    ];

    state.notifications = vec![
        Notification {
            id: 1,
            severity: NotificationSeverity::Info,
            title: "同期完了".into(),
            message: "3 ファイルを転送しました。".into(),
            actions: vec![NotificationAction::Dismiss],
            created_at: Instant::now()
                .checked_sub(Duration::from_secs(30))
                .unwrap_or_else(Instant::now),
            expires_at: Some(Instant::now() + Duration::from_secs(7)),
        },
        Notification {
            id: 2,
            severity: NotificationSeverity::Error,
            title: "競合を検出".into(),
            message: "同名ファイルが存在します。".into(),
            actions: vec![
                NotificationAction::Retry(JobId(77)),
                NotificationAction::SkipAndContinue(JobId(77)),
                NotificationAction::AbortAll(JobId(77)),
            ],
            created_at: Instant::now()
                .checked_sub(Duration::from_secs(10))
                .unwrap_or_else(Instant::now),
            expires_at: None,
        },
    ];

    state.jobs = vec![JobDisplayInfo {
        id: JobId(77),
        description: "NAS にコピー".into(),
        state: JobState::Running,
        progress: ProgressInfo {
            bytes_completed: 1_048_576,
            bytes_total: Some(4_194_304),
            files_completed: 1,
            files_total: Some(4),
        },
        speed_samples: VecDeque::new(),
        speed_bps: Some(512_000.0),
    }];

    state.preview_target_path = Some("/Users/test/Pictures/photo.jpg".into());
    state.preview_loading = false;
    state.preview_failed = false;
    state.preview_data = Some(PreviewData::Text {
        content: "first line\nsecond line\nthird line".into(),
    });

    let mapping = sample_remote_mapping();
    state.remote_transport_mappings.push(mapping.clone());
    state.remote_transport_edit_buffer = Some(mapping);

    state
}

pub fn state_with_settings_dialog(section: SettingsSection) -> AppState {
    let mut state = sample_state();
    state.active_dialog = Some(DialogState::Settings {
        selected_section: section,
        cache_encryption_policy: state.cache_encryption_policy,
        map_tile_fetch_enabled: state.map_tile_fetch_enabled,
        theme_choice: state.theme_choice,
        locale_choice: iris_ui_shell::config::LocaleChoice::from_locale(state.locale.as_deref()),
        notification_timeout_input: state.notification_timeout_secs.to_string(),
        shortcut_overrides_json_input: "[]".into(),
        shortcut_overrides_error: None,
        remote_copy_undo_enabled: state.remote_copy_undo_enabled,
        preview_position: state.preview_position,
        properties_position: state.properties_position,
        thumbnail_cache_limit_input: "48".into(),
        memory_budget_limit_input: "256".into(),
        storage_usage: Some(iris_ui_shell::state::StorageUsage {
            thumbnail_bytes: 7 * 1024 * 1024,
            ..iris_ui_shell::state::StorageUsage::default()
        }),
        network_form: sample_network_form(),
        cloud_form: Box::new(sample_cloud_form()),
    });
    state
}

pub fn state_with_network_dialog() -> AppState {
    let mut state = sample_state();
    state.active_dialog = Some(DialogState::ConnectNetwork {
        form: sample_network_form(),
    });
    state
}

pub fn state_with_cloud_dialog() -> AppState {
    let mut state = sample_state();
    state.active_dialog = Some(DialogState::ConnectCloud {
        form: sample_cloud_form(),
    });
    state
}

pub fn state_with_conflict_dialog() -> AppState {
    let mut state = sample_state();
    state.active_dialog = Some(DialogState::Conflict {
        source: object_ref("local-main", "/Users/test/Pictures/photo.jpg"),
        destination_path: "/Volumes/NAS/photo.jpg".into(),
        source_size: 512_000,
        destination_size: 256_000,
        source_mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        destination_mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_300)),
        remember_choice: false,
    });
    state
}

pub fn state_with_delete_dialog() -> AppState {
    let mut state = sample_state();
    state.active_dialog = Some(DialogState::DeleteConfirmation {
        targets: vec![object_ref("local-main", "/Users/test/Pictures/photo.jpg")],
        target_names: vec!["photo.jpg".into()],
        use_trash: true,
        can_use_trash: true,
    });
    state
}

pub fn state_with_create_folder_dialog() -> AppState {
    let mut state = sample_state();
    state.active_dialog = Some(DialogState::CreateFolder {
        input_value: "新しいフォルダ".into(),
    });
    state
}

pub fn state_with_path_bar_edit() -> AppState {
    let mut state = sample_state();
    state.pane_mut(0).path_editing = true;
    state.pane_mut(0).path_input = "~/Projects".into();
    state
}

pub fn state_with_error_dialog() -> AppState {
    let mut state = sample_state();
    state.active_dialog = Some(DialogState::Error {
        title: "接続失敗".into(),
        message: "資格情報を確認してください。".into(),
    });
    state
}

pub fn sample_network_form() -> NetworkConnectFormState {
    NetworkConnectFormState {
        server: "nas.local".into(),
        share_path: "photo".into(),
        username: "iris".into(),
        available_shares: Some(vec!["photo".into(), "backup".into()]),
        ..NetworkConnectFormState::default()
    }
}

pub fn sample_cloud_form() -> CloudConnectFormState {
    CloudConnectFormState {
        display_name: "Production Bucket".into(),
        s3_bucket: "iris-assets".into(),
        s3_region: "ap-northeast-1".into(),
        s3_prefix: "team/shared".into(),
        ..CloudConnectFormState::default()
    }
}

pub fn sample_remote_mapping() -> iris_remote_transport::RemoteTransportMapping {
    iris_remote_transport::RemoteTransportMapping {
        id: "mapping-1".into(),
        local_mount_path: "/Volumes/NAS".into(),
        host: "nas.local".into(),
        ssh_port: 2222,
        ssh_username: "iris".into(),
        remote_base_path: "/volume1/photo".into(),
        transport: RemoteTransportKind::Sftp,
        auth: SshAuthKind::Agent,
        ssh_identity_file: String::new(),
        ssh_password: String::new(),
        enabled: true,
        auto_detected: false,
        notes: "Primary NAS".into(),
    }
}

fn sample_pane(current_path: &str, history: &[&str], entries: Vec<ListEntry>) -> PaneState {
    let mut pane = PaneState::default();
    pane.current_path = current_path.into();
    pane.replace_entries(entries);
    pane.load_state = iris_ui_shell::state::PaneLoadState::Loaded;
    pane.navigation_history = history.iter().map(|path| (*path).to_string()).collect();
    pane.history_index = pane.navigation_history.len();
    pane.breadcrumb_segments =
        iris_ui_shell::state::local_breadcrumbs(std::path::Path::new(&pane.current_path));
    pane
}

fn pane_snapshot(pane: &PaneState) -> PaneSnapshot {
    PaneSnapshot {
        current_path: pane.current_path.clone(),
        entry_names: pane
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect(),
        selected_names: pane.selected_names(),
        focused_index: pane.focused_index,
        history: pane.navigation_history.clone(),
        history_index: pane.history_index,
        view_mode: format!("{:?}", pane.view_mode),
        sort_key: pane.sort_key.to_string(),
        sort_order: format!("{:?}", pane.sort_order),
        error_message: pane.load_state.error_message().map(str::to_owned),
    }
}

fn dialog_snapshot(dialog: Option<&DialogState>) -> Option<DialogSnapshot> {
    dialog.map(|dialog| match dialog {
        DialogState::DeleteConfirmation {
            target_names,
            use_trash,
            ..
        } => DialogSnapshot {
            kind: "DeleteConfirmation".into(),
            summary: format!(
                "targets={}, use_trash={}",
                target_names.join(", "),
                use_trash
            ),
        },
        DialogState::CreateFolder { input_value } => DialogSnapshot {
            kind: "CreateFolder".into(),
            summary: input_value.clone(),
        },
        DialogState::Error { title, message } => DialogSnapshot {
            kind: "Error".into(),
            summary: format!("{title}: {message}"),
        },
        DialogState::Conflict {
            destination_path,
            remember_choice,
            ..
        } => DialogSnapshot {
            kind: "Conflict".into(),
            summary: format!(
                "destination={}, remember={}",
                destination_path, remember_choice
            ),
        },
        DialogState::Settings {
            selected_section,
            thumbnail_cache_limit_input,
            storage_usage,
            remote_copy_undo_enabled,
            ..
        } => {
            let total_storage = storage_usage.as_ref().map(|usage| usage.total_bytes());
            DialogSnapshot {
                kind: "Settings".into(),
                summary: format!(
                    "section={selected_section:?}, limit={thumbnail_cache_limit_input}, storage_total={total_storage:?}, remote_undo={remote_copy_undo_enabled}"
                ),
            }
        },
        DialogState::ConnectNetwork { form } => DialogSnapshot {
            kind: "ConnectNetwork".into(),
            summary: format!("server={}, share={}", form.server, form.share_path),
        },
        DialogState::ConnectCloud { form } => DialogSnapshot {
            kind: "ConnectCloud".into(),
            summary: format!("bucket={}, prefix={}", form.s3_bucket, form.s3_prefix),
        },
        DialogState::BatchRename { state } => DialogSnapshot {
            kind: "BatchRename".into(),
            summary: format!(
                "pane={}, items={}, mode={:?}, can_apply={}",
                state.pane,
                state.items.len(),
                state.mode,
                state.can_apply()
            ),
        },
        DialogState::RemoteUndoConfirmation { item_count, .. } => DialogSnapshot {
            kind: "RemoteUndoConfirmation".into(),
            summary: format!("items={item_count}"),
        },
    })
}

fn clipboard_snapshot(state: &AppState) -> Option<String> {
    state.clipboard.as_ref().map(|clipboard| match clipboard {
        iris_ui_shell::state::ClipboardContent::Copy(items) => {
            format!("Copy({})", items.len())
        }
        iris_ui_shell::state::ClipboardContent::Cut(items) => format!("Cut({})", items.len()),
    })
}

fn preview_summary(preview: Option<&PreviewData>) -> Option<String> {
    preview.map(|preview| match preview {
        PreviewData::Image { width, height, .. } => {
            format!("Image({width:?}x{height:?})")
        }
        PreviewData::Video { duration_secs, .. } => format!("Video({duration_secs:?})"),
        PreviewData::Pdf { page_count, .. } => format!("Pdf({page_count:?})"),
        PreviewData::Text { content } => format!("Text(lines={})", content.lines().count()),
        PreviewData::Unknown { file_type, size } => format!("Unknown({file_type}, {size})"),
    })
}

fn list_entry(backend_id: &str, display_path: &str, is_dir: bool, size: Option<u64>) -> ListEntry {
    let name = PathBuf::from(display_path)
        .file_name()
        .and_then(|name| name.to_str())
        .expect("file name")
        .to_string();
    ListEntry {
        metadata_level: MetadataLevel::L1,
        size,
        mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        ctime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_900_000)),
        ..ListEntry::new(object_ref(backend_id, display_path), name, is_dir)
    }
}

fn object_ref(backend_id: &str, display_path: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId(backend_id.into()),
            stable_id: StableId(display_path.into()),
        },
        parent_stable_id: None,
        display_path: display_path.into(),
    }
}

fn snapshot_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.json"))
}
