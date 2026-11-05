mod cache;
mod layout;
mod navigation;
mod selection;
mod settings;
mod tasks;

use crate::config::{PaneTabConfig, ShortcutOverride, ThemeChoice};
use crate::input::dnd::{DndState, DndTarget};
use crate::remote_transport::RemoteTransportMapping;
use crate::undo::UndoJournal;
use iced::keyboard::Modifiers;
use iris_thumb_preview::scheduler::ThumbnailScheduler;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use cache::{
    memory_budget_limit_input_from_bytes, parse_memory_budget_limit_input_mib,
    parse_thumbnail_cache_limit_input_mib, thumbnail_cache_limit_input_from_bytes, CachedThumbnail,
    EntryCacheStamp, FolderStats, PreviewData, QueuedThumbnailRequest, StorageUsage,
    ThumbnailCacheKey, ThumbnailCacheLru, DEFAULT_MEMORY_BUDGET_LIMIT_BYTES,
    DEFAULT_THUMBNAIL_CACHE_LIMIT_BYTES,
};
pub use layout::{
    ListColumnKind, ListColumnSpec, PreviewPosition, PropertiesPosition, ResizeState, ResizeTarget,
    SortKey, SortOrder, ViewMode,
};
pub(crate) use navigation::list_entry_kind;
pub use navigation::{BreadcrumbSegment, EntryIndex, PaneLoadState, PaneState, PathSuggestion};
pub use settings::{
    normalize_notification_timeout_secs, notification_timeout_input_from_secs,
    parse_notification_timeout_input_secs, parse_smb_port_input, CacheEncryptionPolicy,
    CloudConnectFormState, CloudProvider, NetworkConnectFormState, NetworkProtocol,
    SettingsSection, SidebarDropTarget, SidebarGroup, SidebarGroupKind, SidebarItem,
    DEFAULT_NOTIFICATION_TIMEOUT_SECS, MAX_NOTIFICATION_TIMEOUT_SECS,
};
pub use tasks::{
    BatchRenameDialogState, BatchRenameEntry, BatchRenameMode, BatchRenamePreviewItem,
    ClipboardContent, ContextMenuKind, ContextMenuState, DialogState, JobDisplayInfo, Notification,
    NotificationAction, NotificationSeverity, PendingFileItem, PendingFileOperation,
    PendingUndoAction,
};

pub struct PaneTabs {
    tabs: Vec<PaneState>,
    active: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabDragState {
    pub pane: usize,
    pub from: usize,
    pub target: usize,
}

impl Default for PaneTabs {
    fn default() -> Self {
        Self {
            tabs: vec![PaneState::default()],
            active: 0,
        }
    }
}

impl PaneTabs {
    pub fn active(&self) -> &PaneState {
        &self.tabs[self.active]
    }

    pub fn active_mut(&mut self) -> &mut PaneState {
        &mut self.tabs[self.active]
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn tabs(&self) -> &[PaneState] {
        &self.tabs
    }
}

pub struct AppState {
    panes: [PaneTabs; 2],
    pub active_pane: usize,

    pub keyboard_modifiers: Modifiers,
    pub dnd_state: DndState,
    pub tab_drag_state: Option<TabDragState>,
    pub pending_external_drop_paths: Vec<PathBuf>,
    pub pending_external_drop_target: Option<DndTarget>,
    pub pending_external_drop_token: u64,

    pub window_width: f32,
    pub window_height: f32,
    pub window_x: Option<f32>,
    pub window_y: Option<f32>,
    pub cache_encryption_policy: CacheEncryptionPolicy,
    pub map_tile_fetch_enabled: bool,
    pub show_hidden_files: bool,
    pub theme_choice: ThemeChoice,
    pub system_theme: iced::Theme,
    pub locale: Option<String>,
    pub shortcut_overrides: Vec<ShortcutOverride>,
    pub shortcut_recording_action: Option<String>,
    pub command_palette_open: bool,
    pub command_palette_query: String,
    pub command_palette_selected: usize,
    pub remote_copy_undo_enabled: bool,

    pub thumbnail_cache: HashMap<ThumbnailCacheKey, CachedThumbnail>,
    pub thumbnail_cache_lru: ThumbnailCacheLru,
    pub thumbnail_cache_bytes: usize,
    pub thumbnail_cache_limit_bytes: usize,
    pub memory_budget_limit_bytes: usize,
    pub thumbnail_generation_queue: VecDeque<ThumbnailCacheKey>,
    pub thumbnail_generation_queued: HashSet<ThumbnailCacheKey>,
    pub thumbnail_generation_requests: HashMap<ThumbnailCacheKey, QueuedThumbnailRequest>,
    pub thumbnail_schedulers: HashMap<String, ThumbnailScheduler>,
    pub thumbnail_inflight: HashSet<ThumbnailCacheKey>,
    pub thumbnail_inflight_backends: HashMap<ThumbnailCacheKey, String>,
    pub thumbnail_canceled: HashSet<ThumbnailCacheKey>,
    pub thumbnail_unsupported: HashSet<ThumbnailCacheKey>,

    pub sidebar_visible: bool,
    pub preview_panel_visible: bool,
    pub properties_panel_visible: bool,
    pub quick_look_open: bool,
    pub responsive_sidebar_dismissed: bool,
    pub responsive_right_panel_dismissed: bool,

    pub sidebar_width: f32,
    pub right_panel_width: f32,
    pub preview_panel_height: f32,
    pub properties_split_ratio: f32,
    pub properties_position: PropertiesPosition,
    pub resize_state: Option<ResizeState>,
    pub mouse_position: Option<iced::Point>,
    pub last_config_save: Option<Instant>,
    pub config_save_generation: u64,
    /// Set by the first layout load; later loads (the file changed on disk, for instance by
    /// another Iris window) keep this window's tabs and position.
    pub layout_restored: bool,
    /// Started for a folder given on the command line ("Open in New Window").
    pub launched_with_path: bool,
    pub preview_position: PreviewPosition,

    pub dual_pane_enabled: bool,
    pub pane_split_ratio: f32,

    pub sidebar_groups: Vec<SidebarGroup>,
    pub sidebar_renaming_group: Option<usize>,
    pub sidebar_rename_input: String,
    pub sidebar_jobs_visible: bool,
    pub sidebar_dragging_item: Option<(usize, usize)>,
    pub sidebar_dragging_group: Option<usize>,
    pub sidebar_drop_target: Option<SidebarDropTarget>,

    pub clipboard: Option<ClipboardContent>,

    pub active_dialog: Option<DialogState>,
    pub context_menu: Option<ContextMenuState>,

    pub jobs: Vec<JobDisplayInfo>,

    pub pending_operation: Option<PendingFileOperation>,
    pub undo_journal: UndoJournal,

    pub notifications: Vec<Notification>,
    pub next_notification_id: u64,
    pub notification_timeout_secs: u64,

    pub preview_data: Option<PreviewData>,
    pub preview_target_path: Option<String>,
    pub preview_target_stamp: Option<EntryCacheStamp>,
    pub preview_request_version: u64,
    pub preview_loading: bool,
    pub preview_failed: bool,

    pub focused_folder_stats: Option<FolderStats>,
    pub focused_folder_stats_path: Option<String>,

    pub remote_transport_mappings: Vec<RemoteTransportMapping>,
    pub remote_transport_edit_buffer: Option<RemoteTransportMapping>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            panes: [PaneTabs::default(), PaneTabs::default()],
            active_pane: 0,
            keyboard_modifiers: Modifiers::default(),
            dnd_state: DndState::default(),
            tab_drag_state: None,
            pending_external_drop_paths: Vec::new(),
            pending_external_drop_target: None,
            pending_external_drop_token: 0,
            window_width: 1280.0,
            window_height: 800.0,
            window_x: None,
            window_y: None,
            cache_encryption_policy: CacheEncryptionPolicy::Auto,
            map_tile_fetch_enabled: false,
            show_hidden_files: false,
            theme_choice: ThemeChoice::System,
            // iced reports the real system mode after the runtime starts.
            system_theme: iced::Theme::Dark,
            locale: None,
            shortcut_overrides: Vec::new(),
            shortcut_recording_action: None,
            command_palette_open: false,
            command_palette_query: String::new(),
            command_palette_selected: 0,
            remote_copy_undo_enabled: true,
            thumbnail_cache: HashMap::new(),
            thumbnail_cache_lru: ThumbnailCacheLru::default(),
            thumbnail_cache_bytes: 0,
            thumbnail_cache_limit_bytes: DEFAULT_THUMBNAIL_CACHE_LIMIT_BYTES,
            memory_budget_limit_bytes: DEFAULT_MEMORY_BUDGET_LIMIT_BYTES,
            thumbnail_generation_queue: VecDeque::new(),
            thumbnail_generation_queued: HashSet::new(),
            thumbnail_generation_requests: HashMap::new(),
            thumbnail_schedulers: HashMap::new(),
            thumbnail_inflight: HashSet::new(),
            thumbnail_inflight_backends: HashMap::new(),
            thumbnail_canceled: HashSet::new(),
            thumbnail_unsupported: HashSet::new(),
            sidebar_visible: true,
            preview_panel_visible: false,
            properties_panel_visible: false,
            quick_look_open: false,
            responsive_sidebar_dismissed: false,
            responsive_right_panel_dismissed: false,
            sidebar_width: 200.0,
            right_panel_width: 280.0,
            preview_panel_height: 300.0,
            properties_split_ratio: 0.5,
            properties_position: PropertiesPosition::BelowPreview,
            resize_state: None,
            mouse_position: None,
            last_config_save: None,
            config_save_generation: 0,
            layout_restored: false,
            launched_with_path: false,
            preview_position: PreviewPosition::Right,
            dual_pane_enabled: false,
            pane_split_ratio: 0.5,
            sidebar_groups: Vec::new(),
            sidebar_renaming_group: None,
            sidebar_rename_input: String::new(),
            sidebar_jobs_visible: false,
            sidebar_dragging_item: None,
            sidebar_dragging_group: None,
            sidebar_drop_target: None,
            clipboard: None,
            active_dialog: None,
            context_menu: None,
            jobs: Vec::new(),
            pending_operation: None,
            undo_journal: UndoJournal::default(),
            notifications: Vec::new(),
            next_notification_id: 1,
            notification_timeout_secs: DEFAULT_NOTIFICATION_TIMEOUT_SECS,
            preview_data: None,
            preview_target_path: None,
            preview_target_stamp: None,
            preview_request_version: 0,
            preview_loading: false,
            preview_failed: false,
            focused_folder_stats: None,
            focused_folder_stats_path: None,
            remote_transport_mappings: Vec::new(),
            remote_transport_edit_buffer: None,
        }
    }
}

pub fn theme_from_mode(mode: iced::theme::Mode) -> iced::Theme {
    match mode {
        iced::theme::Mode::Light => iced::Theme::Light,
        iced::theme::Mode::Dark | iced::theme::Mode::None => iced::Theme::Dark,
    }
}

impl AppState {
    pub fn active_pane(&self) -> &PaneState {
        self.panes[self.active_pane].active()
    }

    pub fn active_pane_mut(&mut self) -> &mut PaneState {
        self.panes[self.active_pane].active_mut()
    }

    pub fn pane(&self, idx: usize) -> &PaneState {
        self.panes[idx].active()
    }

    pub fn pane_checked(&self, idx: usize) -> Option<&PaneState> {
        self.panes.get(idx).map(PaneTabs::active)
    }

    pub fn pane_mut(&mut self, idx: usize) -> &mut PaneState {
        self.panes[idx].active_mut()
    }

    pub fn pane_tabs(&self, idx: usize) -> &PaneTabs {
        &self.panes[idx]
    }

    pub fn active_panes(&self) -> [&PaneState; 2] {
        [self.pane(0), self.pane(1)]
    }

    /// Every tab of both panes as `(pane index, is the pane's active tab, tab)`.
    pub fn all_tabs_mut(&mut self) -> impl Iterator<Item = (usize, bool, &mut PaneState)> {
        self.panes
            .iter_mut()
            .enumerate()
            .flat_map(|(pane_idx, tabs)| {
                let active = tabs.active;
                tabs.tabs
                    .iter_mut()
                    .enumerate()
                    .map(move |(tab_idx, tab)| (pane_idx, tab_idx == active, tab))
            })
    }

    pub fn open_tab(&mut self, pane_idx: usize) -> usize {
        let current = self.panes[pane_idx].active();
        let mut tab = PaneState {
            current_path: current.current_path.clone(),
            view_mode: current.view_mode.clone(),
            sort_key: current.sort_key,
            sort_order: current.sort_order,
            list_columns: current.list_columns.clone(),
            ..PaneState::default()
        };
        if !tab.current_path.is_empty() {
            tab.push_to_history(tab.current_path.clone());
        }

        let tabs = &mut self.panes[pane_idx];
        let insert_at = tabs.active + 1;
        tabs.tabs.insert(insert_at, tab);
        tabs.active = insert_at;
        insert_at
    }

    pub fn close_tab(&mut self, pane_idx: usize, tab_idx: usize) -> bool {
        let tabs = &mut self.panes[pane_idx];
        if tabs.tabs.len() <= 1 || tab_idx >= tabs.tabs.len() {
            return false;
        }

        tabs.tabs.remove(tab_idx);
        if tabs.active > tab_idx {
            tabs.active -= 1;
        } else if tabs.active >= tabs.tabs.len() {
            tabs.active = tabs.tabs.len() - 1;
        }
        true
    }

    pub fn switch_tab(&mut self, pane_idx: usize, tab_idx: usize) -> bool {
        let tabs = &mut self.panes[pane_idx];
        if tab_idx >= tabs.tabs.len() || tabs.active == tab_idx {
            return false;
        }
        tabs.active = tab_idx;
        true
    }

    pub fn switch_to_next_tab(&mut self, pane_idx: usize) -> bool {
        let tabs = &mut self.panes[pane_idx];
        if tabs.tabs.len() <= 1 {
            return false;
        }
        tabs.active = (tabs.active + 1) % tabs.tabs.len();
        true
    }

    pub fn move_tab(&mut self, pane_idx: usize, from: usize, to: usize) -> bool {
        let tabs = &mut self.panes[pane_idx];
        if from >= tabs.tabs.len() || to >= tabs.tabs.len() || from == to {
            return false;
        }

        let tab = tabs.tabs.remove(from);
        tabs.tabs.insert(to, tab);
        if tabs.active == from {
            tabs.active = to;
        } else if from < tabs.active && to >= tabs.active {
            tabs.active -= 1;
        } else if from > tabs.active && to <= tabs.active {
            tabs.active += 1;
        }
        true
    }

    /// Changes whenever a tab is opened, closed, moved, switched or navigated.
    pub fn tab_layout_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for tabs in &self.panes {
            tabs.active.hash(&mut hasher);
            tabs.tabs.len().hash(&mut hasher);
            for tab in &tabs.tabs {
                tab.current_path.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    pub fn pane_tab_configs(&self) -> Vec<PaneTabConfig> {
        self.panes
            .iter()
            .enumerate()
            .flat_map(|(pane_idx, tabs)| {
                tabs.tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, tab)| !tab.current_path.is_empty())
                    .map(move |(tab_idx, tab)| PaneTabConfig {
                        pane: pane_idx,
                        path: tab.current_path.clone(),
                        active: tab_idx == tabs.active,
                    })
            })
            .collect()
    }

    pub fn restore_pane_tabs(&mut self, configs: &[PaneTabConfig]) -> bool {
        let mut restored = false;
        for pane_idx in 0..self.panes.len() {
            let pane_configs = configs
                .iter()
                .filter(|config| config.pane == pane_idx && !config.path.is_empty())
                .collect::<Vec<_>>();
            if pane_configs.is_empty() {
                continue;
            }

            let mut active = pane_configs
                .iter()
                .position(|config| config.active)
                .unwrap_or(0);
            let tabs = pane_configs
                .into_iter()
                .map(|config| {
                    let mut tab = PaneState {
                        current_path: config.path.clone(),
                        ..PaneState::default()
                    };
                    tab.push_to_history(tab.current_path.clone());
                    tab
                })
                .collect::<Vec<_>>();
            active = active.min(tabs.len() - 1);
            self.panes[pane_idx] = PaneTabs { tabs, active };
            restored = true;
        }
        restored
    }

    pub fn add_notification(
        &mut self,
        severity: NotificationSeverity,
        title: String,
        message: String,
        actions: Vec<NotificationAction>,
    ) {
        let now = Instant::now();
        let expires_at = notification_expires_at(now, self.notification_timeout_secs, &actions);
        let notification = Notification {
            id: self.next_notification_id,
            severity,
            title,
            message,
            actions,
            created_at: now,
            expires_at,
        };
        self.next_notification_id += 1;
        self.notifications.push(notification);
    }

    pub fn remove_notification(&mut self, id: u64) {
        self.notifications
            .retain(|notification| notification.id != id);
    }

    pub fn remove_expired_notifications(&mut self) {
        let now = Instant::now();
        self.notifications.retain(|notification| {
            notification
                .expires_at
                .is_none_or(|expires_at| expires_at > now)
        });
    }

    pub fn next_notification_expiry(&self) -> Option<(u64, Duration)> {
        let now = Instant::now();
        let mut next: Option<(&Notification, Instant)> = None;
        for notification in &self.notifications {
            let Some(expires_at) = notification.expires_at else {
                continue;
            };
            if next
                .as_ref()
                .is_none_or(|(_, next_expires_at)| expires_at < *next_expires_at)
            {
                next = Some((notification, expires_at));
            }
        }
        next.map(|(notification, expires_at)| {
            (notification.id, expires_at.saturating_duration_since(now))
        })
    }

    pub fn refresh_notification_expirations(&mut self) {
        let now = Instant::now();
        for notification in &mut self.notifications {
            notification.expires_at =
                notification_expires_at(now, self.notification_timeout_secs, &notification.actions);
        }
    }
}

fn notification_expires_at(
    now: Instant,
    timeout_secs: u64,
    actions: &[NotificationAction],
) -> Option<Instant> {
    if timeout_secs == 0
        || actions
            .iter()
            .any(NotificationAction::requires_user_decision)
    {
        None
    } else {
        Some(now + Duration::from_secs(timeout_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::list::ListEntry;
    use iris_core_domain::types::metadata::MetadataLevel;
    use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
    use iris_core_domain::types::transfer::JobId;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn system_theme_mode_mapping_is_explicit() {
        assert_eq!(
            theme_from_mode(iced::theme::Mode::Light),
            iced::Theme::Light
        );
        assert_eq!(theme_from_mode(iced::theme::Mode::Dark), iced::Theme::Dark);
        assert_eq!(theme_from_mode(iced::theme::Mode::None), iced::Theme::Dark);
    }

    #[test]
    fn tab_layout_fingerprint_tracks_saved_tab_state_only() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/a".to_string();
        let initial = state.tab_layout_fingerprint();

        state.pane_mut(0).scroll_y = 120.0;
        state.show_hidden_files = true;
        assert_eq!(state.tab_layout_fingerprint(), initial);

        state.pane_mut(0).current_path = "/b".to_string();
        let navigated = state.tab_layout_fingerprint();
        assert_ne!(navigated, initial);

        state.open_tab(0);
        let opened = state.tab_layout_fingerprint();
        assert_ne!(opened, navigated);

        assert!(state.switch_tab(0, 0));
        assert_ne!(state.tab_layout_fingerprint(), opened);
    }

    #[test]
    fn dismissible_notifications_expire_after_configured_timeout() {
        let mut state = AppState {
            notification_timeout_secs: 12,
            ..AppState::default()
        };

        state.add_notification(
            NotificationSeverity::Info,
            "Done".to_string(),
            String::new(),
            vec![NotificationAction::Dismiss],
        );

        let notification = &state.notifications[0];
        let expires_at = notification.expires_at.expect("expiration");
        assert!(expires_at > notification.created_at);
        assert!(expires_at.duration_since(notification.created_at) >= Duration::from_secs(12));
    }

    #[test]
    fn action_notifications_do_not_expire() {
        let mut state = AppState::default();

        state.add_notification(
            NotificationSeverity::Error,
            "Conflict".to_string(),
            String::new(),
            vec![NotificationAction::Retry(JobId(1))],
        );

        assert!(state.notifications[0].expires_at.is_none());
    }

    #[test]
    fn remove_expired_notifications_keeps_persistent_toasts() {
        let mut state = AppState::default();
        state.add_notification(
            NotificationSeverity::Info,
            "Old".to_string(),
            String::new(),
            vec![NotificationAction::Dismiss],
        );
        state.add_notification(
            NotificationSeverity::Error,
            "Needs input".to_string(),
            String::new(),
            vec![NotificationAction::AbortAll(JobId(2))],
        );
        state.notifications[0].expires_at = Some(Instant::now() - Duration::from_secs(1));

        state.remove_expired_notifications();

        assert_eq!(state.notifications.len(), 1);
        assert_eq!(state.notifications[0].title, "Needs input");
    }

    #[test]
    fn tabs_open_next_to_active_and_inherit_display_state() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.current_path = "/tmp/source".to_string();
            pane.view_mode = ViewMode::Grid;
            pane.sort_key = SortKey::Size;
            pane.sort_order = SortOrder::Descending;
            pane.set_list_column_width(ListColumnKind::Size, 123.0);
            pane.set_list_column_width(ListColumnKind::Date, 234.0);
        }

        let tab_idx = state.open_tab(0);

        assert_eq!(tab_idx, 1);
        assert_eq!(state.pane_tabs(0).active_index(), 1);
        assert_eq!(state.pane_tabs(0).tabs().len(), 2);
        assert_eq!(state.pane(0).current_path, "/tmp/source");
        assert_eq!(state.pane(0).view_mode, ViewMode::Grid);
        assert_eq!(state.pane(0).sort_key, SortKey::Size);
        assert_eq!(state.pane(0).sort_order, SortOrder::Descending);
        assert_eq!(state.pane(0).list_column_width(ListColumnKind::Size), 123.0);
        assert_eq!(state.pane(0).list_column_width(ListColumnKind::Date), 234.0);
        assert_eq!(state.pane(0).navigation_history, vec!["/tmp/source"]);
    }

    #[test]
    fn tabs_close_adjusts_active_index_without_closing_last_tab() {
        let mut state = AppState::default();
        state.open_tab(0);
        state.open_tab(0);

        assert_eq!(state.pane_tabs(0).active_index(), 2);
        assert!(state.close_tab(0, 1));
        assert_eq!(state.pane_tabs(0).active_index(), 1);
        assert_eq!(state.pane_tabs(0).tabs().len(), 2);

        assert!(state.close_tab(0, 1));
        assert_eq!(state.pane_tabs(0).active_index(), 0);
        assert_eq!(state.pane_tabs(0).tabs().len(), 1);
        assert!(!state.close_tab(0, 0));
        assert_eq!(state.pane_tabs(0).tabs().len(), 1);
    }

    #[test]
    fn tabs_switch_next_wraps_active_tab() {
        let mut state = AppState::default();
        state.open_tab(0);

        assert!(state.switch_to_next_tab(0));
        assert_eq!(state.pane_tabs(0).active_index(), 0);
        assert!(state.switch_to_next_tab(0));
        assert_eq!(state.pane_tabs(0).active_index(), 1);
        assert!(state.switch_tab(0, 0));
        assert_eq!(state.pane_tabs(0).active_index(), 0);
        assert!(!state.switch_tab(0, 9));
    }

    #[test]
    fn tabs_move_preserves_active_tab_identity() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/tmp/a".to_string();
        state.open_tab(0);
        state.pane_mut(0).current_path = "/tmp/b".to_string();
        state.open_tab(0);
        state.pane_mut(0).current_path = "/tmp/c".to_string();

        assert_eq!(state.pane_tabs(0).active_index(), 2);
        assert!(state.move_tab(0, 2, 1));
        assert_eq!(state.pane_tabs(0).active_index(), 1);
        assert_eq!(state.pane(0).current_path, "/tmp/c");

        assert!(state.move_tab(0, 0, 2));
        assert_eq!(state.pane_tabs(0).active_index(), 0);
        assert_eq!(state.pane(0).current_path, "/tmp/c");
        assert!(!state.move_tab(0, 9, 0));
    }

    #[test]
    fn tabs_restore_from_config_and_export_non_empty_paths() {
        let mut state = AppState::default();
        let configs = vec![
            PaneTabConfig {
                pane: 0,
                path: "/tmp/a".to_string(),
                active: false,
            },
            PaneTabConfig {
                pane: 0,
                path: "/tmp/b".to_string(),
                active: true,
            },
            PaneTabConfig {
                pane: 1,
                path: String::new(),
                active: true,
            },
            PaneTabConfig {
                pane: 1,
                path: "/tmp/c".to_string(),
                active: true,
            },
        ];

        assert!(state.restore_pane_tabs(&configs));
        assert_eq!(state.pane_tabs(0).tabs().len(), 2);
        assert_eq!(state.pane_tabs(0).active_index(), 1);
        assert_eq!(state.pane(0).current_path, "/tmp/b");
        assert_eq!(state.pane_tabs(1).tabs().len(), 1);
        assert_eq!(state.pane(1).current_path, "/tmp/c");

        let exported = state.pane_tab_configs();
        assert_eq!(exported.len(), 3);
        assert!(exported
            .iter()
            .any(|tab| tab.pane == 0 && tab.path == "/tmp/b" && tab.active));
        assert!(exported
            .iter()
            .any(|tab| tab.pane == 1 && tab.path == "/tmp/c" && tab.active));
    }

    #[test]
    fn filter_select_all_and_focus_use_visible_entries() {
        let mut pane = PaneState {
            entries: vec![
                test_entry("alpha.txt"),
                test_entry("bravo.log"),
                test_entry("alphabet.md"),
                test_entry("charlie.txt"),
            ],
            ..PaneState::default()
        };

        pane.set_filter_query("alpha".to_string());
        assert_eq!(pane.visible_indices(), &[0, 2]);

        pane.select_all();
        assert_eq!(pane.selected_indices, [0, 2].into_iter().collect());
        assert_eq!(pane.focused_index, Some(0));

        pane.focus_next();
        assert_eq!(pane.focused_index, Some(2));
        pane.focus_previous();
        assert_eq!(pane.focused_index, Some(0));
    }

    #[test]
    fn filter_matching_normalizes_unicode_to_nfc() {
        let mut pane = PaneState {
            entries: vec![test_entry("café.txt"), test_entry("resume.txt")],
            ..PaneState::default()
        };
        pane.set_filter_query("cafe\u{301}".to_string());

        assert_eq!(pane.visible_indices(), &[0]);
    }

    #[test]
    fn filter_range_selection_skips_hidden_entries() {
        let mut pane = PaneState {
            entries: vec![
                test_entry("alpha.txt"),
                test_entry("bravo.log"),
                test_entry("alpine.png"),
                test_entry("charlie.txt"),
            ],
            ..PaneState::default()
        };

        pane.set_filter_query("alp".to_string());
        pane.select_single(0);
        pane.select_range_to(2, false);

        assert_eq!(pane.selected_indices, [0, 2].into_iter().collect());
        assert_eq!(pane.focused_index, Some(2));
    }

    #[test]
    fn filter_tokens_apply_to_visible_entries() {
        let mut photo = test_entry("photo.jpg");
        photo.size = Some(3 * 1024 * 1024);
        let mut pdf = test_entry("scan.pdf");
        pdf.size = Some(512 * 1024);
        let mut video = test_entry("clip.mp4");
        video.size = Some(8 * 1024 * 1024);
        let mut pane = PaneState {
            entries: vec![photo, pdf, video],
            ..PaneState::default()
        };

        pane.set_filter_query("kind:image size:>1MiB gps:absent".to_string());

        assert_eq!(pane.visible_indices(), &[0]);
        assert!(pane.filter_warnings.is_empty());
    }

    #[test]
    fn filter_tokens_keep_parse_warnings() {
        let mut pane = PaneState {
            entries: vec![test_entry("photo.jpg")],
            ..PaneState::default()
        };

        pane.set_filter_query("kind:nope photo".to_string());

        assert_eq!(pane.visible_indices(), &[0]);
        assert_eq!(pane.filter_warnings.len(), 1);
    }

    #[test]
    fn date_sort_uses_modified_time_until_capture_time_is_available() {
        let mut older = test_entry("older.jpg");
        older.mtime = Some(UNIX_EPOCH + Duration::from_secs(10));
        let mut newer = test_entry("newer.jpg");
        newer.mtime = Some(UNIX_EPOCH + Duration::from_secs(20));
        let mut pane = PaneState {
            entries: vec![newer, older],
            sort_key: SortKey::Date,
            ..PaneState::default()
        };

        pane.sort_entries();

        assert_eq!(
            pane.visible_entries()
                .map(|(_, entry)| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["older.jpg", "newer.jpg"]
        );
        assert_eq!(
            pane.entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["newer.jpg", "older.jpg"]
        );
    }

    #[test]
    fn sort_entries_projects_visible_order_without_remapping_selection() {
        let file_alpha = test_entry("alpha.txt");
        let mut dir_beta = test_entry("beta");
        dir_beta.is_dir = true;
        let file_charlie = test_entry("charlie.txt");
        let mut pane = PaneState {
            entries: vec![file_alpha, dir_beta, file_charlie],
            sort_key: SortKey::Name,
            sort_order: SortOrder::Descending,
            selected_indices: [0, 2].into_iter().collect(),
            focused_index: Some(2),
            ..PaneState::default()
        };

        pane.sort_entries();

        assert_eq!(
            pane.visible_entries()
                .map(|(_, entry)| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta", "charlie.txt", "alpha.txt"]
        );
        assert_eq!(
            pane.entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha.txt", "beta", "charlie.txt"]
        );
        assert_eq!(pane.selected_indices, [0, 2].into_iter().collect());
        assert_eq!(pane.focused_index, Some(2));
    }

    #[test]
    fn append_entries_merges_into_visible_projection_without_reordering_entries() {
        let mut pane = PaneState {
            entries: vec![test_entry("mango.txt"), test_entry("zulu.txt")],
            sort_key: SortKey::Name,
            ..PaneState::default()
        };
        pane.sort_entries();

        pane.append_entries(vec![test_entry("alpha.txt"), test_entry("omega.txt")]);

        assert_eq!(
            pane.visible_entries()
                .map(|(_, entry)| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha.txt", "mango.txt", "omega.txt", "zulu.txt"]
        );
        assert_eq!(
            pane.entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mango.txt", "zulu.txt", "alpha.txt", "omega.txt"]
        );
    }

    #[test]
    fn append_entries_respects_active_filter_projection() {
        let mut pane = PaneState {
            entries: vec![test_entry("alpha-one.txt"), test_entry("bravo.txt")],
            ..PaneState::default()
        };
        pane.set_filter_query("alpha".to_string());

        pane.append_entries(vec![test_entry("charlie.txt"), test_entry("alpha-two.txt")]);

        assert_eq!(pane.visible_indices(), &[0, 3]);
        assert_eq!(
            pane.visible_entries()
                .map(|(_, entry)| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha-one.txt", "alpha-two.txt"]
        );
    }

    #[test]
    fn entry_projection_rebuilds_strictly_increasing_list_row_offsets() {
        let mut pane = PaneState::default();
        pane.replace_entries(vec![
            test_entry("alpha.txt"),
            test_entry("bravo.txt"),
            test_entry("charlie.txt"),
        ]);

        assert_eq!(pane.visible_list_row_offsets.len(), 4);
        assert!(pane
            .visible_list_row_offsets
            .windows(2)
            .all(|pair| pair[1] > pair[0]));
        assert_eq!(
            pane.list_rows_height(),
            3.0 * crate::widgets::metrics::LIST_ROW_HEIGHT
        );

        pane.append_entries(vec![test_entry("delta.txt")]);
        pane.set_filter_query("a".to_string());

        assert_eq!(
            pane.visible_list_row_offsets.len(),
            pane.visible_indices().len() + 1
        );
        assert!(pane
            .visible_list_row_offsets
            .windows(2)
            .all(|pair| pair[1] > pair[0]));
    }

    #[test]
    fn visible_positions_track_sort_append_and_filter_projection() {
        let mut pane = PaneState {
            entries: vec![test_entry("bravo.txt"), test_entry("delta.txt")],
            sort_key: SortKey::Name,
            ..PaneState::default()
        };
        pane.sort_entries();

        assert_eq!(pane.visible_position(0), Some(0));
        assert_eq!(pane.visible_position(1), Some(1));

        pane.append_entries(vec![test_entry("alpha.txt"), test_entry("charlie.txt")]);

        assert_eq!(pane.visible_indices(), &[2, 0, 3, 1]);
        assert_eq!(pane.visible_position(2), Some(0));
        assert_eq!(pane.visible_position(0), Some(1));
        assert_eq!(pane.visible_position(3), Some(2));
        assert_eq!(pane.visible_position(1), Some(3));

        pane.set_filter_query("ha".to_string());

        assert_eq!(pane.visible_indices(), &[2, 3]);
        assert_eq!(pane.visible_position(2), Some(0));
        assert_eq!(pane.visible_position(3), Some(1));
        assert_eq!(pane.visible_position(1), None);
        assert_eq!(pane.visible_position(0), None);
    }

    #[test]
    fn grid_visible_sections_track_sort_append_and_filter_projection() {
        let mut dir_zeta = test_entry("zeta");
        dir_zeta.is_dir = true;
        let file_alpha = test_entry("alpha.txt");
        let mut dir_beta = test_entry("beta");
        dir_beta.is_dir = true;
        let file_gamma = test_entry("gamma.txt");
        let mut pane = PaneState {
            entries: vec![dir_zeta, file_alpha, dir_beta, file_gamma],
            sort_key: SortKey::Name,
            ..PaneState::default()
        };
        pane.sort_entries();

        assert_eq!(pane.visible_indices(), &[2, 0, 1, 3]);
        assert_eq!(pane.visible_folder_indices(), &[2, 0]);
        assert_eq!(pane.visible_file_indices(), &[1, 3]);

        let mut dir_delta = test_entry("delta");
        dir_delta.is_dir = true;
        pane.append_entries(vec![dir_delta, test_entry("bravo.txt")]);

        assert_eq!(pane.visible_folder_indices(), &[2, 4, 0]);
        assert_eq!(pane.visible_file_indices(), &[1, 5, 3]);

        pane.set_filter_query("bravo".to_string());

        assert!(pane.visible_folder_indices().is_empty());
        assert_eq!(pane.visible_file_indices(), &[5]);
    }

    #[test]
    fn entry_path_index_tracks_replace_append_and_clear() {
        let mut pane = PaneState::default();
        pane.replace_entries(vec![test_entry("alpha.txt"), test_entry("bravo.txt")]);

        assert_eq!(pane.entry_index_by_display_path("/tmp/alpha.txt"), Some(0));
        assert_eq!(pane.entry_index_by_display_path("/tmp/bravo.txt"), Some(1));

        pane.append_entries(vec![test_entry("charlie.txt")]);
        pane.selection_anchor_path = Some("/tmp/charlie.txt".to_string());

        assert_eq!(
            pane.entry_index_by_display_path("/tmp/charlie.txt"),
            Some(2)
        );
        assert_eq!(pane.selection_anchor_index(), Some(2));

        pane.clear_entries();

        assert_eq!(pane.entry_index_by_display_path("/tmp/alpha.txt"), None);
        assert_eq!(pane.selection_anchor_index(), None);
        assert!(pane.visible_indices().is_empty());
    }

    #[test]
    fn entry_path_index_supports_mutable_lookup() {
        let mut pane = PaneState::default();
        pane.replace_entries(vec![test_entry("alpha.txt"), test_entry("bravo.txt")]);

        let entry = pane.entry_mut_by_display_path("/tmp/bravo.txt").unwrap();
        entry.size = Some(42);

        assert_eq!(pane.entries[1].size, Some(42));
        assert_eq!(pane.entry_index_by_display_path("/tmp/bravo.txt"), Some(1));
    }

    #[test]
    fn entry_object_key_index_tracks_replace_append_and_clear() {
        let mut pane = PaneState::default();
        let alpha = test_entry("alpha.txt");
        let bravo = test_entry("bravo.txt");
        let alpha_key = alpha.object_ref.key.clone();
        let bravo_key = bravo.object_ref.key.clone();
        pane.replace_entries(vec![alpha, bravo]);

        assert_eq!(pane.entry_index_by_object_key(&alpha_key), Some(0));
        assert_eq!(pane.entry_index_by_object_key(&bravo_key), Some(1));

        let charlie = test_entry("charlie.txt");
        let charlie_key = charlie.object_ref.key.clone();
        pane.append_entries(vec![charlie]);

        assert_eq!(pane.entry_index_by_object_key(&charlie_key), Some(2));

        pane.clear_entries();

        assert_eq!(pane.entry_index_by_object_key(&alpha_key), None);
        assert_eq!(pane.entry_index_by_object_key(&charlie_key), None);
    }

    #[test]
    fn append_entries_merges_non_name_sort_keys() {
        let mut small = test_entry("small.bin");
        small.size = Some(10);
        let mut large = test_entry("large.bin");
        large.size = Some(30);
        let mut middle = test_entry("middle.bin");
        middle.size = Some(20);
        let mut pane = PaneState {
            entries: vec![small, large],
            sort_key: SortKey::Size,
            sort_order: SortOrder::Descending,
            ..PaneState::default()
        };
        pane.sort_entries();

        pane.append_entries(vec![middle]);

        assert_eq!(
            pane.visible_entries()
                .map(|(_, entry)| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["large.bin", "middle.bin", "small.bin"]
        );
    }

    proptest::proptest! {
        #[test]
        fn filtered_selection_and_focus_stay_visible(
            names in proptest::collection::vec("[a-z]{1,8}", 1..32),
            query in "[a-z]{0,4}"
        ) {
            let mut pane = PaneState {
                entries: names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| test_entry(&format!("{name}-{index}.txt")))
                    .collect(),
                ..PaneState::default()
            };
            pane.select_all();
            pane.focused_index = Some(0);
            pane.set_filter_query(query);

            let visible = pane.visible_indices().iter().copied().collect::<std::collections::HashSet<_>>();
            proptest::prop_assert!(visible
                .iter()
                .all(|index| (*index as usize) < pane.entries.len()));
            proptest::prop_assert!(pane.selected_indices.iter().all(|index| visible.contains(index)));
            if let Some(focused) = pane.focused_index {
                proptest::prop_assert!(visible.contains(&focused));
            }
        }
    }

    fn test_entry(name: &str) -> ListEntry {
        let display_path = format!("/tmp/{name}");
        ListEntry {
            object_ref: ObjectRef {
                key: ObjectKey {
                    backend_id: BackendId("local".to_string()),
                    stable_id: StableId(display_path.clone()),
                },
                parent_stable_id: None,
                display_path,
            },
            sort_name_key: ListEntry::sort_name_key_for(name),
            sort_extension_key: ListEntry::sort_extension_key_for(name),
            name: name.to_string(),
            is_dir: false,
            metadata_level: MetadataLevel::L1,
            size: None,
            mtime: None,
            ctime: None,
            is_symlink: false,
            symlink_target: None,
            is_inaccessible: false,
            annotation: None,
        }
    }
}
