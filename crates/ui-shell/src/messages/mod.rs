pub mod pane;

pub use pane::{EntryMetadataUpdate, PaneMessage};

use crate::config::{LayoutConfig, LocaleChoice, ThemeChoice};
use crate::input::dnd::DndTarget;
use crate::runtime::ConflictMetadata;
use crate::state::{
    BatchRenameMode, CacheEncryptionPolicy, CloudProvider, NetworkProtocol, PreviewData,
    PreviewPosition, ResizeTarget, SettingsSection, SidebarItem, ThumbnailCacheKey,
};
use crate::undo::{UndoItem, UndoOpKind};
use iced::keyboard;
use iced::widget::image as iced_image;
use iced::window;
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::progress::ProgressInfo;
use iris_core_domain::types::transfer::{JobId, JobState};
use iris_platform_integration::mount::MountedVolume;
use iris_transfer::job_manager::RestoredJob;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ThumbnailData {
    pub handle: iced_image::Handle,
    pub memory_len: usize,
}

/// Completion of one pending file operation item.
#[derive(Debug, Clone)]
pub struct PendingItemResult {
    pub bytes_completed: u64,
    /// Usually 1; an rsync batch reports every file it transferred.
    pub files_completed: u64,
}

#[derive(Debug, Clone)]
pub enum NavigationMessage {
    Back,
    Forward,
    Up,
    EditPath { pane: usize },
    CopyTextToClipboard(String),
}

#[derive(Debug, Clone)]
pub enum DirectoryMessage {
    RefreshRequested {
        pane_idx: usize,
        path: String,
    },
    Polled {
        path: String,
        entries: Vec<ListEntry>,
    },
    LoadFailed {
        path: String,
        error: String,
    },
    MountedVolumesChanged(Vec<MountedVolume>),
    CloudBackendsLoaded(Vec<SidebarItem>),
    RestoredJobsLoaded(Vec<RestoredJob>),
}

#[derive(Debug, Clone)]
pub enum DialogMessage {
    Close,
    InputChanged(String),
}

#[derive(Debug, Clone)]
pub enum ConflictMessage {
    Skip,
    Rename,
    Overwrite,
    SkipAll,
    OverwriteAll,
    ToggleRememberChoice,
}

#[derive(Debug, Clone)]
pub enum JobMessage {
    /// The job was written to the job store and its transfers may start.
    Persisted(JobId),
    Pause(JobId),
    Resume(JobId),
    Cancel(JobId),
    Retry(JobId),
    RuntimeProgress(JobId, ProgressInfo),
    RuntimeState(JobId, JobState),
}

#[derive(Debug, Clone)]
pub enum SidebarMessage {
    ToggleVisibility,
    ToggleGroupCollapsed(usize),
    AddCurrentPathToGroup(usize),
    AddCurrentPathToFavorites,
    RemoveItem { group: usize, item: usize },
    AddGroup,
    BeginRenameGroup(usize),
    ConfirmRenameGroup(String),
    CancelRenameGroup,
    RenameInput(String),
    ToggleJobs,
    DragItem(usize, usize),
    DragGroup(usize),
    DragOver(Option<crate::state::SidebarDropTarget>),
    Drop,
    DragCancel,
    OpenInNewWindow(String),
}

#[derive(Debug, Clone)]
pub enum ConnectionMessage {
    SetNetworkProtocol(NetworkProtocol),
    SetCloudProvider(CloudProvider),
    NetworkServerChanged(String),
    NetworkSmbPortChanged(String),
    NetworkShareChanged(String),
    NetworkUsernameChanged(String),
    NetworkSmbDomainChanged(String),
    NetworkSmbPasswordChanged(String),
    NetworkSmbAllowGuestChanged(bool),
    CloudDisplayNameChanged(String),
    CloudS3BucketChanged(String),
    CloudS3RegionChanged(String),
    CloudS3EndpointChanged(String),
    CloudS3ConditionalWritesConfirmed(bool),
    CloudS3PrefixChanged(String),
    CloudPCloudClientIdChanged(String),
    CloudPCloudRedirectUrlChanged(String),
    CloudPCloudCallbackChanged(String),
    StartCloudAuthorization,
    BrowseNetworkShares,
    NetworkSharesLoaded(Result<Vec<String>, String>),
    NetworkShareSelected(String),
    PerformConnectNetwork,
    PerformConnectCloud,
    ConnectNetworkCompleted(Result<SidebarItem, String>),
    ConnectCloudCompleted(Result<SidebarItem, String>),
    UnmountVolume(String),
    UnmountVolumeCompleted(Result<String, String>),
}

#[derive(Debug, Clone)]
pub enum SettingsMessage {
    Open,
    SelectSection(SettingsSection),
    SetThemeChoice(ThemeChoice),
    SetLocaleChoice(LocaleChoice),
    SetNotificationTimeoutInput(String),
    SetShortcutOverridesJsonInput(String),
    BeginShortcutRecording(String),
    CancelShortcutRecording,
    ClearShortcutOverride(String),
    RecordedShortcut(String),
    SetRemoteCopyUndoEnabled(bool),
    SetCacheEncryptionPolicy(CacheEncryptionPolicy),
    SetMapTileFetchEnabled(bool),
    SetThumbnailCacheLimitInput(String),
    SetMemoryBudgetLimitInput(String),
    StorageUsageCalculated(crate::state::StorageUsage),
    ClearCache,
    CacheClearCompleted(Result<(), String>),
    OpenLogFolder,
    OpenLogFolderCompleted(Result<(), String>),
}

#[derive(Debug, Clone)]
pub enum RemoteTransportMessage {
    Redetect,
    RedetectCompleted(Vec<crate::remote_transport::RemoteTransportMapping>),
    AddNew,
    Remove(String),
    ToggleEnabled(String),
    BeginEdit(String),
    CancelEdit,
    SaveEdit,
    SecretDeleted(Result<(), String>),
    EditField(crate::remote_transport::EditField, String),
    EditTransport(crate::remote_transport::RemoteTransportKind),
    EditAuth(crate::remote_transport::SshAuthKind),
}

#[derive(Debug, Clone)]
pub enum TabMessage {
    New { pane: usize },
    Duplicate { pane: usize, tab: usize },
    Close { pane: usize, tab: usize },
    Switch { pane: usize, tab: usize },
    SwitchNext { pane: usize },
    MoveLeft { pane: usize, tab: usize },
    MoveRight { pane: usize, tab: usize },
    BeginDrag { pane: usize, tab: usize },
    DragOver { pane: usize, tab: usize },
    Drop,
}

#[derive(Debug, Clone, Copy)]
pub enum AppMenuCommand {
    OpenSelected,
    CreateFolder,
    EditPath,
    NavigateBack,
    NavigateForward,
    NavigateUp,
    CutSelected,
    CopySelected,
    Paste,
    Undo,
    Redo,
    DeleteSelected,
    SelectAll,
    OpenSettings,
    Refresh,
    ToggleSidebar,
    TogglePreviewPanel,
    TogglePropertiesPanel,
    ToggleHiddenFiles,
    OpenHelp,
}

#[derive(Debug, Clone)]
pub enum Message {
    Pane(usize, PaneMessage),

    Navigation(NavigationMessage),

    Directory(DirectoryMessage),

    CopySelected,
    CutSelected,
    Paste,
    DeleteSelected,
    Undo,
    ConfirmRemoteUndo,
    Redo,
    OpenSelected,
    CreateFolder,
    Rename(crate::state::EntryIndex),
    BatchRenameSelected,
    BatchRenameModeChanged(BatchRenameMode),
    BatchRenamePatternChanged(String),
    BatchRenameReplacementChanged(String),
    BatchRenameStartChanged(String),
    BatchRenamePaddingChanged(String),
    BatchRenameDateFormatChanged(String),
    PerformBatchRename,
    PerformCreateFolder(String),
    PerformDelete,
    SetDeleteUseTrash(bool),
    OpenItemContextMenu {
        pane: usize,
        index: crate::state::EntryIndex,
    },
    OpenSidebarContextMenu {
        group: usize,
        item: usize,
    },
    OpenTabContextMenu {
        pane: usize,
        tab: usize,
    },
    CloseContextMenu,
    ItemActionOpen,
    ItemActionCopy,
    ItemActionCut,
    ItemActionRename,
    ItemActionDelete,
    SidebarContextOpen,
    SidebarContextOpenInNewWindow,

    Dialog(DialogMessage),
    Conflict(ConflictMessage),

    /// (job_id, item_count, result)
    RsyncBatchCompleted(JobId, usize, Result<PendingItemResult, String>),

    Job(JobMessage),

    Sidebar(SidebarMessage),

    Connection(ConnectionMessage),

    Tab(TabMessage),

    TogglePreviewPanel,
    ToggleQuickLook,
    CloseQuickLook,
    QuickLookSelectNext,
    QuickLookSelectPrevious,
    TogglePropertiesPanel,
    PreviewDebounced(u64),
    PreviewLoaded(String, Option<PreviewData>),
    SetPreviewPosition(PreviewPosition),
    SetPropertiesPosition(crate::state::PropertiesPosition),
    ConfigFileChanged,
    FolderStatsLoaded(String, crate::state::FolderStats),

    ResizeStart(ResizeTarget),
    MouseMoved(iced::Point),
    ResizeEnd,

    ToggleDualPane,
    SetActivePane(usize),
    DismissResponsiveSidebar,
    DismissResponsiveRightPanel,
    ActivatePaneAndRun {
        pane: usize,
        message: Box<Message>,
    },
    CopyToOtherPane,
    MoveToOtherPane,

    Settings(SettingsMessage),

    SaveLayoutConfig,
    FlushLayoutConfig(u64),
    LayoutConfigLoaded(LayoutConfig),
    LayoutConfigSaved(Result<(), String>),

    DismissNotification(u64),
    SkipError(JobId),
    AbortJob(JobId),

    ThumbnailReady(ThumbnailCacheKey, ThumbnailData),
    ThumbnailFailed(ThumbnailCacheKey),
    ThumbnailUnsupported(ThumbnailCacheKey),

    DragStart {
        pane: usize,
        index: crate::state::EntryIndex,
    },
    DragHover(DndTarget),
    DragLeave(DndTarget),
    DragEnd,

    KeyPressed(keyboard::Key, keyboard::Modifiers),
    ModifiersChanged(keyboard::Modifiers),
    OpenCommandPalette,
    CloseCommandPalette,
    CommandPaletteInputChanged(String),
    CommandPaletteMoveNext,
    CommandPaletteMovePrevious,
    ExecuteCommandPaletteSelection,
    ExecuteCommandPaletteIndex(usize),

    AppMenuCommand(AppMenuCommand),
    OpenHelpCompleted(Result<(), String>),

    RemoteTransport(RemoteTransportMessage),

    RenameCompleted {
        original_path: PathBuf,
        renamed_path: PathBuf,
        result: Result<(), String>,
    },
    BatchRenameCompleted {
        items: Vec<UndoItem>,
        failures: Vec<String>,
    },
    CreateFolderCompleted {
        path: PathBuf,
        result: Result<(), String>,
    },
    DeleteCompleted {
        trashed_items: Vec<crate::undo::UndoItem>,
        result: Result<(), String>,
    },
    RecordUndoOp {
        kind: UndoOpKind,
        items: Vec<UndoItem>,
    },
    UndoCompleted {
        op: crate::undo::UndoableOp,
        redo_op: Option<crate::undo::UndoableOp>,
        result: Result<(), String>,
    },
    RedoCompleted {
        op: crate::undo::UndoableOp,
        undo_op: Option<crate::undo::UndoableOp>,
        result: Result<(), String>,
    },
    OpenObjectCompleted(Result<(), String>),
    ConflictCheckCompleted(JobId, usize, Result<Option<ConflictMetadata>, String>),
    /// Job, item index, outcome.
    PendingItemCompleted(JobId, usize, Result<PendingItemResult, String>),

    ToggleHiddenFiles,
    PruneExpiredNotifications,
    SystemThemeChanged(iced::theme::Mode),
    FocusFilter,
    ClearFilter,
    Refresh,
    WindowResized(iced::Size),
    WindowEvent(window::Id, window::Event),
    CommitExternalDrop(u64),
    BeginWindowDrag,
}

impl From<NavigationMessage> for Message {
    fn from(value: NavigationMessage) -> Self {
        Self::Navigation(value)
    }
}

impl From<DirectoryMessage> for Message {
    fn from(value: DirectoryMessage) -> Self {
        Self::Directory(value)
    }
}

impl From<DialogMessage> for Message {
    fn from(value: DialogMessage) -> Self {
        Self::Dialog(value)
    }
}

impl From<ConflictMessage> for Message {
    fn from(value: ConflictMessage) -> Self {
        Self::Conflict(value)
    }
}

impl From<JobMessage> for Message {
    fn from(value: JobMessage) -> Self {
        Self::Job(value)
    }
}

impl From<SidebarMessage> for Message {
    fn from(value: SidebarMessage) -> Self {
        Self::Sidebar(value)
    }
}

impl From<ConnectionMessage> for Message {
    fn from(value: ConnectionMessage) -> Self {
        Self::Connection(value)
    }
}

impl From<SettingsMessage> for Message {
    fn from(value: SettingsMessage) -> Self {
        Self::Settings(value)
    }
}

impl From<RemoteTransportMessage> for Message {
    fn from(value: RemoteTransportMessage) -> Self {
        Self::RemoteTransport(value)
    }
}

impl From<TabMessage> for Message {
    fn from(value: TabMessage) -> Self {
        Self::Tab(value)
    }
}
