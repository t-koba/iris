use crate::config::{LocaleChoice, ThemeChoice};
use crate::undo::UndoableOp;

use super::layout::{PreviewPosition, PropertiesPosition};
use super::settings::{
    CacheEncryptionPolicy, CloudConnectFormState, NetworkConnectFormState, SettingsSection,
};
use super::StorageUsage;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::options::ConflictResolution;
use iris_core_domain::types::progress::ProgressInfo;
use iris_core_domain::types::transfer::{JobId, JobState};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

#[derive(Clone, Debug)]
pub enum ClipboardContent {
    Copy(Vec<ObjectRef>),
    Cut(Vec<ObjectRef>),
}

#[derive(Clone, Debug)]
pub enum DialogState {
    DeleteConfirmation {
        targets: Vec<ObjectRef>,
        target_names: Vec<String>,
        use_trash: bool,
        can_use_trash: bool,
    },
    CreateFolder {
        input_value: String,
    },
    Error {
        title: String,
        message: String,
    },
    Conflict {
        source: ObjectRef,
        destination_path: String,
        source_size: u64,
        destination_size: u64,
        source_mtime: Option<SystemTime>,
        destination_mtime: Option<SystemTime>,
        remember_choice: bool,
    },
    Settings {
        selected_section: SettingsSection,
        cache_encryption_policy: CacheEncryptionPolicy,
        map_tile_fetch_enabled: bool,
        theme_choice: ThemeChoice,
        locale_choice: LocaleChoice,
        notification_timeout_input: String,
        shortcut_overrides_json_input: String,
        shortcut_overrides_error: Option<String>,
        remote_copy_undo_enabled: bool,
        preview_position: PreviewPosition,
        properties_position: PropertiesPosition,
        thumbnail_cache_limit_input: String,
        memory_budget_limit_input: String,
        storage_usage: Option<StorageUsage>,
        network_form: NetworkConnectFormState,
        cloud_form: Box<CloudConnectFormState>,
    },
    ConnectNetwork {
        form: NetworkConnectFormState,
    },
    ConnectCloud {
        form: CloudConnectFormState,
    },
    BatchRename {
        state: BatchRenameDialogState,
    },
    RemoteUndoConfirmation {
        op: UndoableOp,
        item_count: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchRenameMode {
    NumberSequence,
    Replace,
    Regex,
    Date,
}

impl BatchRenameMode {
    pub const ALL: [Self; 4] = [Self::NumberSequence, Self::Replace, Self::Regex, Self::Date];

    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::NumberSequence => "batch-rename-mode-number",
            Self::Replace => "batch-rename-mode-replace",
            Self::Regex => "batch-rename-mode-regex",
            Self::Date => "batch-rename-mode-date",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BatchRenameDialogState {
    pub pane: usize,
    pub items: Vec<BatchRenameEntry>,
    pub existing_paths: Vec<PathBuf>,
    pub mode: BatchRenameMode,
    pub pattern_input: String,
    pub replacement_input: String,
    pub start_input: String,
    pub padding_input: String,
    pub date_format_input: String,
    pub preview: Vec<BatchRenamePreviewItem>,
    pub error_message: Option<String>,
}

impl BatchRenameDialogState {
    pub fn can_apply(&self) -> bool {
        self.error_message.is_none()
            && self.preview.iter().all(|item| !item.blocking)
            && self.preview.iter().any(|item| item.status.is_none())
    }
}

#[derive(Clone, Debug)]
pub struct BatchRenameEntry {
    pub target: ObjectRef,
    pub original_name: String,
    pub original_path: PathBuf,
    pub mtime: Option<SystemTime>,
}

#[derive(Clone, Debug)]
pub struct BatchRenamePreviewItem {
    pub original_name: String,
    pub new_name: String,
    pub status: Option<String>,
    pub blocking: bool,
}

#[derive(Clone, Debug)]
pub enum ContextMenuKind {
    Item {
        pane: usize,
        index: super::EntryIndex,
    },
    SidebarItem {
        group: usize,
        item: usize,
        removable: bool,
    },
    Tab {
        pane: usize,
        tab: usize,
        can_close: bool,
        can_move_left: bool,
        can_move_right: bool,
    },
}

#[derive(Clone, Debug)]
pub struct ContextMenuState {
    pub kind: ContextMenuKind,
    pub target_name: String,
    pub position: iced::Point,
}

#[derive(Clone, Debug)]
pub struct JobDisplayInfo {
    pub id: JobId,
    pub description: String,
    pub state: JobState,
    pub progress: ProgressInfo,
    pub speed_samples: VecDeque<(Instant, u64)>,
    pub speed_bps: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct PendingFileItem {
    pub source: ObjectRef,
    pub destination_path: String,
}

/// File operation in progress (paste or drag and drop).
#[derive(Clone, Debug)]
pub struct PendingFileOperation {
    pub items: Vec<PendingFileItem>,
    /// Next item that has never been started.
    pub current_index: usize,
    pub is_move: bool,
    pub remember_resolution: Option<ConflictResolution>,
    pub job_id: JobId,
    pub in_flight: usize,
    pub rsync_failed: bool,
    pub undo_action: Option<PendingUndoAction>,
    /// Items that failed and wait for Retry, Skip or Abort; nothing new starts meanwhile.
    pub failed: Vec<usize>,
    /// Failed items the user chose to retry; they start before new items.
    pub retry: Vec<usize>,
    /// Items transferred at the same time.
    pub max_in_flight: usize,
    /// The job is in the job store, so its tasks can be updated and resumed after a restart.
    pub persisted: bool,
}

impl PendingFileOperation {
    pub fn new(
        job_id: JobId,
        items: Vec<PendingFileItem>,
        is_move: bool,
        undo_action: Option<PendingUndoAction>,
        max_in_flight: usize,
    ) -> Self {
        Self {
            items,
            current_index: 0,
            is_move,
            remember_resolution: None,
            job_id,
            in_flight: 0,
            rsync_failed: false,
            undo_action,
            failed: Vec::new(),
            retry: Vec::new(),
            max_in_flight: max_in_flight.max(1),
            persisted: false,
        }
    }

    /// The item to start next: a retried one first, then the next new one.
    pub fn next_index(&self) -> Option<usize> {
        self.retry
            .last()
            .copied()
            .or_else(|| (self.current_index < self.items.len()).then_some(self.current_index))
    }

    /// Marks [`Self::next_index`] as started.
    pub fn take_next_index(&mut self) -> Option<usize> {
        if let Some(index) = self.retry.pop() {
            return Some(index);
        }
        let index = (self.current_index < self.items.len()).then_some(self.current_index)?;
        self.current_index += 1;
        Some(index)
    }

    /// Every item has been started at least once and none waits for a retry.
    pub fn all_started(&self) -> bool {
        self.retry.is_empty() && self.current_index >= self.items.len()
    }
}

#[derive(Clone, Debug)]
pub enum PendingUndoAction {
    RecordTransfer,
    RecordOp(UndoableOp),
    PushRedo(UndoableOp),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub enum NotificationAction {
    Dismiss,
    Retry(JobId),
    SkipAndContinue(JobId),
    AbortAll(JobId),
}

impl NotificationAction {
    /// The job a decision button acts on.
    pub fn job_id(&self) -> Option<&JobId> {
        match self {
            NotificationAction::Retry(job_id)
            | NotificationAction::SkipAndContinue(job_id)
            | NotificationAction::AbortAll(job_id) => Some(job_id),
            NotificationAction::Dismiss => None,
        }
    }

    pub fn requires_user_decision(&self) -> bool {
        matches!(
            self,
            NotificationAction::Retry(_)
                | NotificationAction::SkipAndContinue(_)
                | NotificationAction::AbortAll(_)
        )
    }
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    pub severity: NotificationSeverity,
    pub title: String,
    pub message: String,
    pub actions: Vec<NotificationAction>,
    pub created_at: Instant,
    pub expires_at: Option<Instant>,
}
