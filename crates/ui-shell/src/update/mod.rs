mod connections;
mod dialogs;
mod directory;
mod file_actions;
pub mod file_ops;
pub mod helpers;
mod interactions;
mod jobs;
pub(crate) mod metadata_hydration;
mod navigation;
pub mod pane;
pub mod preview;
mod remote_transport;
mod settings;
mod sidebar;
mod tabs;
pub mod thumbnail;
mod undo_ops;
mod workspace;

use crate::config::LayoutConfig;
use crate::i18n::tr;
use crate::input::dnd::{DndState, DndTarget};
use crate::messages::{
    AppMenuCommand, DirectoryMessage, Message, NavigationMessage, PaneMessage, SettingsMessage,
    SidebarMessage,
};
use crate::runtime::app_runtime;
use crate::state::{
    AppState, DialogState, JobDisplayInfo, ListColumnKind, NotificationSeverity, PaneLoadState,
    PendingFileItem, PendingFileOperation, PendingUndoAction, PreviewPosition, ResizeState,
    ResizeTarget, SidebarGroup, SidebarItem,
};
use file_ops::*;
use helpers::*;
use iced::clipboard;
use iced::window;
use iced::Task;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::options::ConflictResolution;
use iris_core_domain::types::progress::ProgressInfo;
use iris_core_domain::types::transfer::JobState;
use preview::{clear_preview_state, refresh_preview_for_selection};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thumbnail::*;

/// Bundled so Help works in installed builds, where the source tree does not exist.
const HELP_DOCUMENT: &str = include_str!("../../../../README.md");

/// Writes the bundled help next to the app's other data (plain text, which every OS can show)
/// and opens it.
fn open_help_document() -> Result<(), String> {
    let dir = dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("iris")
        .join("help");
    let path = dir.join("Iris-Help.txt");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(HELP_DOCUMENT) {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        std::fs::write(&path, HELP_DOCUMENT).map_err(|error| error.to_string())?;
    }
    open::that(&path).map_err(|error| error.to_string())
}

pub fn update(state: &mut AppState, message: Message) -> Task<Message> {
    // Open tabs and their folders are part of the saved layout; persist them whenever they
    // change instead of relying on some later setting change to write them.
    let saves_tabs = !matches!(message, Message::LayoutConfigLoaded(..));
    let tabs_before = state.tab_layout_fingerprint();
    let task = dispatch(state, message);
    if saves_tabs && state.tab_layout_fingerprint() != tabs_before {
        return Task::batch([task, Task::done(Message::SaveLayoutConfig)]);
    }
    task
}

fn dispatch(state: &mut AppState, message: Message) -> Task<Message> {
    let mutation_is_busy = state.pending_operation.is_some()
        && matches!(
            &message,
            Message::Paste
                | Message::DeleteSelected
                | Message::Rename(..)
                | Message::BatchRenameSelected
                | Message::CreateFolder
                | Message::CopyToOtherPane
                | Message::MoveToOtherPane
        );
    if mutation_is_busy {
        state.add_notification(
            NotificationSeverity::Warning,
            tr("notification-operation-busy-title"),
            tr("notification-operation-busy-message"),
            vec![],
        );
        return Task::none();
    }

    if state.context_menu.is_some() && matches!(message, Message::Sidebar(_) | Message::Tab(_)) {
        state.context_menu = None;
    }

    match message {
        Message::ActivatePaneAndRun { pane, message } => {
            state.active_pane = pane.min(1);
            update(state, *message)
        }
        Message::Pane(pane_idx, pane_msg) => pane::handle_pane_message(state, pane_idx, pane_msg),
        Message::Navigation(message) => navigation::handle(state, message),
        Message::Directory(message) => directory::handle(state, message),
        Message::Sidebar(message) => sidebar::handle(state, message),
        Message::Connection(message) => connections::handle(state, message),
        Message::Settings(message) => settings::handle(state, message),
        Message::RemoteTransport(message) => remote_transport::handle(state, message),
        Message::Tab(message) => tabs::handle(state, message),
        Message::DragStart { .. }
        | Message::DragHover(..)
        | Message::DragLeave(..)
        | Message::DragEnd
        | Message::WindowEvent(..)
        | Message::CommitExternalDrop(..)
        | Message::KeyPressed(..)
        | Message::ModifiersChanged(..)
        | Message::Refresh
        | Message::ToggleHiddenFiles
        | Message::WindowResized(..)
        | Message::BeginWindowDrag
        | Message::AppMenuCommand(..)
        | Message::OpenHelpCompleted(..)
        | Message::ConfigFileChanged
        | Message::ResizeStart(..)
        | Message::SystemThemeChanged(..)
        | Message::FocusFilter
        | Message::ClearFilter
        | Message::MouseMoved(..)
        | Message::ResizeEnd => interactions::handle(state, message),
        Message::TogglePreviewPanel
        | Message::ToggleQuickLook
        | Message::CloseQuickLook
        | Message::QuickLookSelectNext
        | Message::QuickLookSelectPrevious
        | Message::TogglePropertiesPanel
        | Message::PreviewDebounced(..)
        | Message::PreviewLoaded(..)
        | Message::DismissNotification(..)
        | Message::PruneExpiredNotifications
        | Message::SkipError(..)
        | Message::AbortJob(..)
        | Message::SetPreviewPosition(..)
        | Message::SetPropertiesPosition(..)
        | Message::FolderStatsLoaded(..)
        | Message::ToggleDualPane
        | Message::SetActivePane(..)
        | Message::DismissResponsiveSidebar
        | Message::DismissResponsiveRightPanel
        | Message::CopyToOtherPane
        | Message::MoveToOtherPane
        | Message::OpenCommandPalette
        | Message::CloseCommandPalette
        | Message::CommandPaletteInputChanged(..)
        | Message::CommandPaletteMoveNext
        | Message::CommandPaletteMovePrevious
        | Message::ExecuteCommandPaletteSelection
        | Message::ExecuteCommandPaletteIndex(..)
        | Message::SaveLayoutConfig
        | Message::FlushLayoutConfig(..)
        | Message::LayoutConfigLoaded(..)
        | Message::LayoutConfigSaved(..) => workspace::handle(state, message),

        Message::CopySelected
        | Message::CutSelected
        | Message::Paste
        | Message::DeleteSelected
        | Message::Undo
        | Message::ConfirmRemoteUndo
        | Message::Redo
        | Message::OpenSelected
        | Message::CreateFolder
        | Message::Rename(..)
        | Message::BatchRenameSelected
        | Message::BatchRenameModeChanged(..)
        | Message::BatchRenamePatternChanged(..)
        | Message::BatchRenameReplacementChanged(..)
        | Message::BatchRenameStartChanged(..)
        | Message::BatchRenamePaddingChanged(..)
        | Message::BatchRenameDateFormatChanged(..)
        | Message::PerformBatchRename
        | Message::RenameCompleted { .. }
        | Message::BatchRenameCompleted { .. }
        | Message::PerformCreateFolder(..)
        | Message::CreateFolderCompleted { .. }
        | Message::PerformDelete
        | Message::OpenObjectCompleted(..)
        | Message::DeleteCompleted { .. }
        | Message::RecordUndoOp { .. }
        | Message::UndoCompleted { .. }
        | Message::RedoCompleted { .. }
        | Message::RsyncBatchCompleted(..)
        | Message::PendingItemCompleted(..)
        | Message::ConflictCheckCompleted(..)
        | Message::SetDeleteUseTrash(..)
        | Message::OpenItemContextMenu { .. }
        | Message::OpenSidebarContextMenu { .. }
        | Message::OpenTabContextMenu { .. }
        | Message::CloseContextMenu
        | Message::ItemActionOpen
        | Message::ItemActionCopy
        | Message::ItemActionCut
        | Message::ItemActionRename
        | Message::ItemActionDelete
        | Message::SidebarContextOpen
        | Message::SidebarContextOpenInNewWindow => file_actions::handle(state, message),

        Message::Dialog(message) => dialogs::handle_dialog(state, message),
        Message::Conflict(message) => dialogs::handle_conflict(state, message),
        Message::Job(message) => jobs::handle(state, message),

        Message::ThumbnailReady(key, thumb) => {
            state.thumbnail_inflight.remove(&key);
            state.thumbnail_inflight_backends.remove(&key);
            if state.thumbnail_canceled.remove(&key) || !thumbnail_cache_key_is_current(state, &key)
            {
                generate_thumbnails(state)
            } else {
                insert_thumbnail_cache(state, key, thumb);
                generate_thumbnails(state)
            }
        }
        Message::ThumbnailFailed(key) => {
            state.thumbnail_inflight.remove(&key);
            state.thumbnail_inflight_backends.remove(&key);
            state.thumbnail_canceled.remove(&key);
            generate_thumbnails(state)
        }
        Message::ThumbnailUnsupported(key) => {
            state.thumbnail_inflight.remove(&key);
            state.thumbnail_inflight_backends.remove(&key);
            if state.thumbnail_canceled.remove(&key) || !thumbnail_cache_key_is_current(state, &key)
            {
                generate_thumbnails(state)
            } else {
                state.thumbnail_unsupported.insert(key);
                generate_thumbnails(state)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::list::ListEntry;
    use iris_core_domain::types::metadata::MetadataLevel;
    use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
    use iris_core_domain::types::transfer::JobId;
    use std::path::PathBuf;

    use crate::state::{PendingFileOperation, PendingUndoAction, ThumbnailCacheKey};
    use crate::undo::{UndoItem, UndoOpKind, UndoableOp};

    #[test]
    fn reset_pane_directory_state_clears_filter_and_selection() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(1);
            pane.entries = vec![test_list_entry("/tmp/alpha.txt", "alpha.txt")];
            pane.set_filter_query("alpha".to_string());
            pane.select_single(0);
            pane.selection_anchor_path = Some("/tmp/alpha.txt".to_string());
            pane.load_state = PaneLoadState::Failed {
                message: "stale".to_string(),
            };
        }

        reset_pane_directory_state(&mut state, 1);

        let pane = state.pane(1);
        assert!(pane.entries.is_empty());
        assert!(pane.filter_query.is_empty());
        assert!(pane.selected_indices.is_empty());
        assert!(pane.selection_anchor_path.is_none());
        assert!(pane.focused_index.is_none());
        assert_eq!(pane.load_state, PaneLoadState::Idle);
    }

    #[test]
    fn activate_pane_and_run_dispatches_global_actions_to_the_clicked_pane() {
        let mut state = AppState::default();
        state.dual_pane_enabled = true;
        state.active_pane = 0;
        state.pane_mut(0).set_filter_query("alpha".to_string());
        state.pane_mut(1).set_filter_query("bravo".to_string());

        let _ = update(
            &mut state,
            Message::ActivatePaneAndRun {
                pane: 1,
                message: Box::new(Message::ClearFilter),
            },
        );

        assert_eq!(state.active_pane, 1);
        assert_eq!(state.pane(0).filter_query, "alpha");
        assert!(state.pane(1).filter_query.is_empty());
    }

    #[test]
    fn unchanged_directory_poll_marks_full_load_complete() {
        let mut state = AppState::default();
        let path = state.pane(0).current_path.clone();
        let entries = vec![test_list_entry("/tmp/alpha.txt", "alpha.txt")];
        {
            let pane = state.pane_mut(0);
            pane.entries = entries.clone();
            pane.load_state = PaneLoadState::LoadingMore;
        }

        let _ = update(
            &mut state,
            DirectoryMessage::Polled { path, entries }.into(),
        );

        assert_eq!(state.pane(0).load_state, PaneLoadState::Loaded);
    }

    #[test]
    fn directory_load_failure_replaces_loading_with_retryable_failed_state() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/tmp/missing".to_string();
        state.pane_mut(0).load_state = PaneLoadState::LoadingInitial;

        let _ = update(
            &mut state,
            DirectoryMessage::LoadFailed {
                path: "/tmp/missing".to_string(),
                error: "not found".to_string(),
            }
            .into(),
        );

        assert_eq!(state.pane(0).load_state.error_message(), Some("not found"));
        assert!(!state.pane(0).load_state.is_loading());
    }

    #[test]
    fn stale_directory_poll_is_ignored() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/tmp/current".to_string();

        let _ = update(
            &mut state,
            DirectoryMessage::Polled {
                path: "/tmp/old".to_string(),
                entries: vec![test_list_entry("/tmp/old/alpha.txt", "alpha.txt")],
            }
            .into(),
        );

        assert!(state.pane(0).entries.is_empty());
        assert_eq!(state.pane(0).load_state, PaneLoadState::Idle);
    }

    #[test]
    fn directory_poll_restores_selection_by_indexed_display_path() {
        let mut state = AppState::default();
        let path = "/tmp/current".to_string();
        {
            let pane = state.pane_mut(0);
            pane.current_path = path.clone();
            pane.replace_entries(vec![
                test_list_entry("/tmp/current/alpha.txt", "alpha.txt"),
                test_list_entry("/tmp/current/bravo.txt", "bravo.txt"),
            ]);
            pane.select_single(1);
        }

        let _ = update(
            &mut state,
            DirectoryMessage::Polled {
                path,
                entries: vec![
                    test_list_entry("/tmp/current/bravo.txt", "bravo.txt"),
                    test_list_entry("/tmp/current/charlie.txt", "charlie.txt"),
                ],
            }
            .into(),
        );

        let pane = state.pane(0);
        assert_eq!(
            pane.entry_index_by_display_path("/tmp/current/bravo.txt"),
            Some(0)
        );
        assert_eq!(pane.selected_indices, [0].into_iter().collect());
        assert_eq!(pane.focused_index, Some(0));
    }

    #[test]
    fn stale_pane_directory_load_is_ignored() {
        let mut state = AppState::default();
        state.pane_mut(1).current_path = "/tmp/current".to_string();

        let _ = update(
            &mut state,
            Message::Pane(
                1,
                PaneMessage::DirectoryLoaded {
                    path: "/tmp/old".to_string(),
                    entries: vec![test_list_entry("/tmp/old/alpha.txt", "alpha.txt")],
                },
            ),
        );

        assert!(state.pane(1).entries.is_empty());
        assert_eq!(state.pane(1).load_state, PaneLoadState::Idle);
    }

    #[test]
    fn directory_batch_load_appends_entries_and_marks_done() {
        let mut state = AppState::default();
        let path = "/tmp/current".to_string();
        state.pane_mut(0).current_path = path.clone();

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::DirectoryHeadLoaded {
                    path: path.clone(),
                    entries: vec![test_list_entry("/tmp/current/beta.txt", "beta.txt")],
                },
            ),
        );
        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::DirectoryBatchLoaded {
                    path,
                    entries: vec![test_list_entry("/tmp/current/alpha.txt", "alpha.txt")],
                    done: true,
                },
            ),
        );

        assert_eq!(
            state
                .pane(0)
                .visible_entries()
                .map(|(_, entry)| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha.txt", "beta.txt"]
        );
        assert_eq!(state.pane(0).load_state, PaneLoadState::Loaded);
    }

    #[test]
    fn stale_directory_batch_load_is_ignored() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/tmp/current".to_string();
        state.pane_mut(0).load_state = PaneLoadState::LoadingInitial;

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::DirectoryBatchLoaded {
                    path: "/tmp/old".to_string(),
                    entries: vec![test_list_entry("/tmp/old/alpha.txt", "alpha.txt")],
                    done: true,
                },
            ),
        );

        assert!(state.pane(0).entries.is_empty());
        assert_eq!(state.pane(0).load_state, PaneLoadState::LoadingInitial);
    }

    #[test]
    fn thumbnail_completion_clears_inflight_backend_metadata() {
        let mut state = AppState::default();
        let entry = test_list_entry("/tmp/photo.jpg", "photo.jpg");
        let key = ThumbnailCacheKey::from_entry(&entry);
        state.thumbnail_inflight.insert(key.clone());
        state
            .thumbnail_inflight_backends
            .insert(key.clone(), "local".to_string());

        let _ = update(&mut state, Message::ThumbnailFailed(key.clone()));

        assert!(!state.thumbnail_inflight.contains(&key));
        assert!(!state.thumbnail_inflight_backends.contains_key(&key));
    }

    #[test]
    fn rename_message_starts_inline_rename_for_focused_row() {
        let mut state = AppState::default();
        state
            .pane_mut(0)
            .replace_entries(vec![test_list_entry("/tmp/old.txt", "old.txt")]);

        let _ = update(&mut state, Message::Rename(0));

        let pane = state.pane(0);
        let rename = pane.inline_rename.as_ref().expect("inline rename");
        assert_eq!(rename.original_name, "old.txt");
        assert_eq!(rename.input_value, "old.txt");
        assert_eq!(rename.target.display_path, "/tmp/old.txt");
        assert!(pane.selected_indices.contains(&0));
        assert_eq!(pane.focused_index, Some(0));
        assert!(state.active_dialog.is_none());
    }

    #[test]
    fn inline_rename_validation_error_stays_inline() {
        let mut state = AppState::default();
        state
            .pane_mut(0)
            .replace_entries(vec![test_list_entry("/tmp/old.txt", "old.txt")]);

        let _ = update(&mut state, Message::Rename(0));
        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::InlineRenameInputChanged {
                    index: 0,
                    value: String::new(),
                },
            ),
        );
        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::CommitInlineRename(0)),
        );

        let rename = state
            .pane(0)
            .inline_rename
            .as_ref()
            .expect("inline rename remains open");
        assert_eq!(rename.input_value, "");
        assert!(rename.error_message.is_some());
    }

    #[test]
    fn toggling_hidden_files_reloads_both_panes() {
        let mut state = AppState::default();
        state.dual_pane_enabled = true;
        state.pane_mut(0).current_path = "/tmp/left".to_string();
        state.pane_mut(1).current_path = "/tmp/right".to_string();
        state.pane_mut(0).load_state = PaneLoadState::Loaded;
        state.pane_mut(1).load_state = PaneLoadState::Loaded;

        let _ = update(&mut state, Message::ToggleHiddenFiles);

        assert!(state.show_hidden_files);
        assert!(state.pane(0).load_state.is_loading());
        assert!(state.pane(1).load_state.is_loading());
    }

    #[test]
    fn inline_rename_rejects_parent_directory_name() {
        let mut state = AppState::default();
        state
            .pane_mut(0)
            .replace_entries(vec![test_list_entry("/tmp/old.txt", "old.txt")]);

        let _ = update(&mut state, Message::Rename(0));
        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::InlineRenameInputChanged {
                    index: 0,
                    value: "..".to_string(),
                },
            ),
        );
        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::CommitInlineRename(0)),
        );

        let rename = state
            .pane(0)
            .inline_rename
            .as_ref()
            .expect("inline rename remains open");
        assert_eq!(
            rename.error_message.as_deref(),
            Some(tr("rename-reserved-name-error").as_str())
        );
    }

    #[test]
    fn create_folder_with_invalid_name_keeps_the_dialog_open() {
        let mut state = AppState::default();
        state.active_dialog = Some(DialogState::CreateFolder {
            input_value: "a/b".to_string(),
        });

        let _ = update(&mut state, Message::PerformCreateFolder("a/b".to_string()));

        assert!(matches!(
            state.active_dialog,
            Some(DialogState::CreateFolder { .. })
        ));
    }

    #[test]
    fn escape_cancels_inline_rename_before_selection_clear() {
        let mut state = AppState::default();
        state
            .pane_mut(0)
            .replace_entries(vec![test_list_entry("/tmp/old.txt", "old.txt")]);
        let _ = update(&mut state, Message::Rename(0));

        let _ = update(
            &mut state,
            Message::KeyPressed(
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                iced::keyboard::Modifiers::default(),
            ),
        );

        assert!(state.pane(0).inline_rename.is_none());
        assert!(state.pane(0).selected_indices.contains(&0));
    }

    #[test]
    fn tab_drag_drop_reorders_tabs_and_preserves_active_identity() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/tmp/a".to_string();
        state.open_tab(0);
        state.pane_mut(0).current_path = "/tmp/b".to_string();
        state.open_tab(0);
        state.pane_mut(0).current_path = "/tmp/c".to_string();

        let _ = update(
            &mut state,
            crate::messages::TabMessage::BeginDrag { pane: 0, tab: 0 }.into(),
        );
        let _ = update(
            &mut state,
            crate::messages::TabMessage::DragOver { pane: 0, tab: 2 }.into(),
        );
        let _ = update(&mut state, crate::messages::TabMessage::Drop.into());

        assert!(state.tab_drag_state.is_none());
        assert_eq!(state.pane_tabs(0).active_index(), 1);
        assert_eq!(state.pane(0).current_path, "/tmp/c");
        assert_eq!(state.pane_tabs(0).tabs()[2].current_path, "/tmp/a");
    }

    #[test]
    fn append_filter_token_preserves_text_and_applies_structured_filter() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.entries = vec![
                test_list_entry("/tmp/photo.jpg", "photo.jpg"),
                test_list_entry("/tmp/report.pdf", "report.pdf"),
            ];
            pane.set_filter_query("photo".to_string());
        }

        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::AppendFilterToken("kind:image".to_string())),
        );

        let pane = state.pane(0);
        assert_eq!(pane.filter_query, "photo kind:image");
        assert_eq!(pane.filter_model.text, "photo");
        assert_eq!(
            pane.filter_model.kinds,
            vec![iris_search::segment::IndexedKind::Image]
        );
        assert_eq!(pane.visible_indices(), &[0]);
    }

    #[test]
    fn date_range_picker_replaces_existing_date_tokens() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.set_filter_query("photo after:2023-01-01 kind:image".to_string());
            pane.filter_date_start_input = "2024-02-03".to_string();
            pane.filter_date_end_input = "2024-02-05".to_string();
        }

        let _ = update(&mut state, Message::Pane(0, PaneMessage::ApplyDateRange));

        let pane = state.pane(0);
        assert_eq!(
            pane.filter_query,
            "text:photo kind:image after:2024-02-03 before:2024-02-05"
        );
        assert_eq!(pane.filter_model.text, "photo");
        assert!(pane.filter_model.date.is_some());
    }

    #[test]
    fn date_range_picker_clear_removes_date_without_losing_other_filters() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.set_filter_query(
                "photo after:2024-02-03 before:2024-02-05 kind:image".to_string(),
            );
            pane.filter_date_start_input = "2024-02-03".to_string();
            pane.filter_date_end_input = "2024-02-05".to_string();
        }

        let _ = update(&mut state, Message::Pane(0, PaneMessage::ClearDateRange));

        let pane = state.pane(0);
        assert_eq!(pane.filter_query, "text:photo kind:image");
        assert!(pane.filter_model.date.is_none());
        assert!(pane.filter_date_start_input.is_empty());
        assert!(pane.filter_date_end_input.is_empty());
    }

    #[test]
    fn invalid_date_range_picker_input_keeps_existing_filter() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.set_filter_query("photo kind:image".to_string());
            pane.filter_date_start_input = "2024-02-30".to_string();
        }

        let _ = update(&mut state, Message::Pane(0, PaneMessage::ApplyDateRange));

        assert_eq!(state.pane(0).filter_query, "photo kind:image");
        assert!(state.pane(0).filter_model.date.is_none());
    }

    #[test]
    fn date_range_calendar_clicks_populate_inputs() {
        let mut state = AppState::default();

        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::SelectDateRangeDay("2024-04-20".to_string())),
        );
        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::SelectDateRangeDay("2024-04-10".to_string())),
        );

        let pane = state.pane(0);
        assert_eq!(pane.filter_date_start_input, "2024-04-10");
        assert_eq!(pane.filter_date_end_input, "2024-04-20");
    }

    #[test]
    fn date_range_calendar_month_navigation_changes_anchor() {
        let mut state = AppState::default();
        state.pane_mut(0).filter_date_anchor =
            Some(chrono::NaiveDate::from_ymd_opt(2024, 4, 15).unwrap());

        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::PreviousDateRangeMonth),
        );
        assert_eq!(
            state.pane(0).filter_date_anchor,
            Some(chrono::NaiveDate::from_ymd_opt(2024, 3, 1).unwrap())
        );

        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::NextDateRangeMonth),
        );
        assert_eq!(
            state.pane(0).filter_date_anchor,
            Some(chrono::NaiveDate::from_ymd_opt(2024, 4, 1).unwrap())
        );
    }

    #[test]
    fn gps_bounds_picker_replaces_existing_gps_filter() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.set_filter_query("photo gps:present kind:image".to_string());
            pane.filter_gps_min_lat_input = "35.0".to_string();
            pane.filter_gps_max_lat_input = "36.0".to_string();
            pane.filter_gps_min_lon_input = "138.5".to_string();
            pane.filter_gps_max_lon_input = "140.0".to_string();
        }

        let _ = update(&mut state, Message::Pane(0, PaneMessage::ApplyGpsBounds));

        let pane = state.pane(0);
        assert_eq!(
            pane.filter_query,
            "text:photo kind:image gps:35,36,138.5,140"
        );
        assert!(matches!(
            pane.filter_model.gps,
            iris_search::filter::GpsFilter::Bounds(_)
        ));
    }

    #[test]
    fn gps_map_selection_updates_inputs_and_filter() {
        let mut state = AppState::default();
        state
            .pane_mut(0)
            .set_filter_query("photo kind:image".to_string());

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::ApplyGpsBoundsFromMap {
                    min_lat: 35.125,
                    max_lat: 36.25,
                    min_lon: 138.5,
                    max_lon: 140.75,
                },
            ),
        );

        let pane = state.pane(0);
        assert_eq!(pane.filter_gps_min_lat_input, "35.125");
        assert_eq!(pane.filter_gps_max_lat_input, "36.25");
        assert_eq!(pane.filter_gps_min_lon_input, "138.5");
        assert_eq!(pane.filter_gps_max_lon_input, "140.75");
        assert_eq!(
            pane.filter_query,
            "text:photo kind:image gps:35.125,36.25,138.5,140.75"
        );
        assert!(matches!(
            pane.filter_model.gps,
            iris_search::filter::GpsFilter::Bounds(_)
        ));
    }

    #[test]
    fn recursive_filter_gps_points_ignore_stale_runs() {
        let mut state = AppState::default();
        let run_id = state.pane_mut(0).begin_recursive_filter().0;

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::RecursiveFilterGpsPoints {
                    run_id: run_id + 1,
                    points: vec![(1.0, 2.0)],
                },
            ),
        );
        assert!(state.pane(0).filter_gps_points.is_empty());

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::RecursiveFilterGpsPoints {
                    run_id,
                    points: vec![(35.5, 139.5)],
                },
            ),
        );
        assert_eq!(state.pane(0).filter_gps_points, vec![(35.5, 139.5)]);
    }

    #[test]
    fn gps_bounds_picker_clear_removes_gps_without_losing_other_filters() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.set_filter_query("photo gps:35,36,138.5,140 kind:image".to_string());
            pane.filter_gps_min_lat_input = "35.0".to_string();
            pane.filter_gps_max_lat_input = "36.0".to_string();
            pane.filter_gps_min_lon_input = "138.5".to_string();
            pane.filter_gps_max_lon_input = "140.0".to_string();
        }

        let _ = update(&mut state, Message::Pane(0, PaneMessage::ClearGpsBounds));

        let pane = state.pane(0);
        assert_eq!(pane.filter_query, "text:photo kind:image");
        assert_eq!(pane.filter_model.gps, iris_search::filter::GpsFilter::Any);
        assert!(pane.filter_gps_min_lat_input.is_empty());
        assert!(pane.filter_gps_max_lat_input.is_empty());
        assert!(pane.filter_gps_min_lon_input.is_empty());
        assert!(pane.filter_gps_max_lon_input.is_empty());
    }

    #[test]
    fn invalid_gps_bounds_picker_input_keeps_existing_filter() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.set_filter_query("photo kind:image".to_string());
            pane.filter_gps_min_lat_input = "91".to_string();
            pane.filter_gps_max_lat_input = "92".to_string();
            pane.filter_gps_min_lon_input = "138.5".to_string();
            pane.filter_gps_max_lon_input = "140.0".to_string();
        }

        let _ = update(&mut state, Message::Pane(0, PaneMessage::ApplyGpsBounds));

        assert_eq!(state.pane(0).filter_query, "photo kind:image");
        assert_eq!(
            state.pane(0).filter_model.gps,
            iris_search::filter::GpsFilter::Any
        );
    }

    #[test]
    fn successful_single_file_messages_record_undo_ops() {
        let mut state = AppState::default();

        let _ = update(
            &mut state,
            Message::RecordUndoOp {
                kind: UndoOpKind::Rename,
                items: vec![UndoItem {
                    source_path: PathBuf::from("/tmp/old.txt"),
                    destination_path: PathBuf::from("/tmp/new.txt"),
                    size: None,
                    mtime: None,
                    trashed_path: None,
                }],
            },
        );
        let rename = state.undo_journal.pop_undo().unwrap();
        assert_eq!(rename.kind, UndoOpKind::Rename);
        assert_eq!(rename.items[0].source_path, PathBuf::from("/tmp/old.txt"));
        assert_eq!(
            rename.items[0].destination_path,
            PathBuf::from("/tmp/new.txt")
        );

        let _ = update(
            &mut state,
            Message::RecordUndoOp {
                kind: UndoOpKind::CreateFolder,
                items: vec![UndoItem {
                    source_path: PathBuf::from("/tmp/new-folder"),
                    destination_path: PathBuf::from("/tmp/new-folder"),
                    size: None,
                    mtime: None,
                    trashed_path: None,
                }],
            },
        );
        let create = state.undo_journal.pop_undo().unwrap();
        assert_eq!(create.kind, UndoOpKind::CreateFolder);
        assert_eq!(
            create.items[0].source_path,
            PathBuf::from("/tmp/new-folder")
        );

        let _ = update(
            &mut state,
            Message::RecordUndoOp {
                kind: UndoOpKind::TrashDelete,
                items: vec![UndoItem {
                    source_path: PathBuf::from("/tmp/deleted.txt"),
                    destination_path: PathBuf::from("/tmp/deleted.txt"),
                    size: None,
                    mtime: None,
                    trashed_path: None,
                }],
            },
        );
        let delete = state.undo_journal.pop_undo().unwrap();
        assert_eq!(delete.kind, UndoOpKind::TrashDelete);
        assert_eq!(
            delete.items[0].source_path,
            PathBuf::from("/tmp/deleted.txt")
        );
    }

    #[test]
    fn failed_single_file_messages_do_not_record_undo_ops() {
        let mut state = AppState::default();

        let _ = update(
            &mut state,
            Message::RenameCompleted {
                original_path: PathBuf::from("/tmp/old.txt"),
                renamed_path: PathBuf::from("/tmp/new.txt"),
                result: Err("failed".to_string()),
            },
        );

        assert!(!state.undo_journal.can_undo());
    }

    #[test]
    fn remote_copy_undo_requires_confirmation_without_popping_stack() {
        let mut state = AppState::default();
        let op = UndoableOp {
            kind: UndoOpKind::Copy,
            items: vec![UndoItem {
                source_path: PathBuf::from("/tmp/source.txt"),
                destination_path: PathBuf::from("/__iris_remote__/s3:test/copied.txt"),
                size: None,
                mtime: None,
                trashed_path: None,
            }],
            performed_at: std::time::SystemTime::UNIX_EPOCH,
        };
        state.undo_journal.record(op);

        let _ = update(&mut state, Message::Undo);

        assert_eq!(state.undo_journal.undo_len(), 1);
        assert!(matches!(
            state.active_dialog,
            Some(DialogState::RemoteUndoConfirmation { item_count: 1, .. })
        ));
    }

    #[test]
    fn disabled_remote_copy_undo_warns_without_popping_stack() {
        let mut state = AppState::default();
        state.remote_copy_undo_enabled = false;
        let op = UndoableOp {
            kind: UndoOpKind::Copy,
            items: vec![UndoItem {
                source_path: PathBuf::from("/tmp/source.txt"),
                destination_path: PathBuf::from("/__iris_remote__/s3:test/copied.txt"),
                size: None,
                mtime: None,
                trashed_path: None,
            }],
            performed_at: std::time::SystemTime::UNIX_EPOCH,
        };
        state.undo_journal.record(op);

        let _ = update(&mut state, Message::Undo);

        assert_eq!(state.undo_journal.undo_len(), 1);
        assert!(state.active_dialog.is_none());
        assert_eq!(state.notifications.len(), 1);
    }

    #[test]
    fn undo_completed_success_shows_notification() {
        let mut state = AppState::default();
        let op = UndoableOp {
            kind: UndoOpKind::Move,
            items: vec![UndoItem {
                source_path: PathBuf::from("/tmp/a"),
                destination_path: PathBuf::from("/tmp/b"),
                size: None,
                mtime: None,
                trashed_path: None,
            }],
            performed_at: std::time::SystemTime::UNIX_EPOCH,
        };

        let _ = update(
            &mut state,
            Message::UndoCompleted {
                op,
                redo_op: None,
                result: Ok(()),
            },
        );

        assert_eq!(state.notifications.len(), 1);
        assert_eq!(state.notifications[0].severity, NotificationSeverity::Info);
    }

    #[test]
    fn pending_completion_moves_transfer_ops_to_expected_stack() {
        let mut state = AppState::default();
        let job_id = JobId(99);
        let op = UndoableOp {
            kind: UndoOpKind::Move,
            items: vec![UndoItem {
                source_path: PathBuf::from("/tmp/a"),
                destination_path: PathBuf::from("/tmp/b"),
                size: None,
                mtime: None,
                trashed_path: None,
            }],
            performed_at: std::time::SystemTime::UNIX_EPOCH,
        };
        state.pending_operation = Some(PendingFileOperation {
            items: Vec::new(),
            current_index: 0,
            is_move: true,
            remember_resolution: None,
            job_id,
            in_flight: 0,
            rsync_failed: false,
            undo_action: Some(PendingUndoAction::PushRedo(op.clone())),
        });

        let _ = process_pending_operation(&mut state);

        assert!(state.undo_journal.can_redo());
        assert_eq!(state.undo_journal.pop_redo().unwrap(), op);
    }

    #[test]
    fn recursive_filter_batches_ignore_stale_runs() {
        let mut state = AppState::default();
        let (run_id, _) = state.pane_mut(0).begin_recursive_filter();
        let stale = run_id.wrapping_add(1);

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::RecursiveFilterBatch {
                    run_id: stale,
                    entries: vec![test_list_entry("/tmp/stale.txt", "stale.txt")],
                    scanned: 1,
                    matched: 1,
                    done: true,
                },
            ),
        );

        assert!(state.pane(0).entries.is_empty());
        assert!(state.pane(0).filter_recursive_running);

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::RecursiveFilterBatch {
                    run_id,
                    entries: vec![test_list_entry("/tmp/live.txt", "live.txt")],
                    scanned: 2,
                    matched: 1,
                    done: true,
                },
            ),
        );

        let pane = state.pane(0);
        assert_eq!(pane.entries.len(), 1);
        assert_eq!(pane.entries[0].name, "live.txt");
        assert_eq!(pane.filter_recursive_scanned, 2);
        assert_eq!(pane.filter_recursive_matched, 1);
        assert!(!pane.filter_recursive_running);
        assert_eq!(pane.load_state, PaneLoadState::Loaded);
    }

    #[test]
    fn recursive_filter_hits_reuse_normal_selection_and_preview_paths() {
        let mut state = AppState::default();
        state.preview_panel_visible = true;
        state.active_pane = 0;
        let (run_id, _) = state.pane_mut(0).begin_recursive_filter();
        let hit = test_list_entry("/tmp/root/nested/report.pdf", "report.pdf");
        let expected_ref = hit.object_ref.clone();

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::RecursiveFilterBatch {
                    run_id,
                    entries: vec![hit],
                    scanned: 10,
                    matched: 1,
                    done: true,
                },
            ),
        );
        let _ = update(&mut state, Message::Pane(0, PaneMessage::SelectItem(0)));

        let selected_refs = state.pane(0).selected_object_refs();
        assert_eq!(selected_refs.len(), 1);
        assert_eq!(selected_refs[0].key, expected_ref.key);
        assert_eq!(
            selected_refs[0].parent_stable_id,
            expected_ref.parent_stable_id
        );
        assert_eq!(selected_refs[0].display_path, expected_ref.display_path);
        assert_eq!(
            state.preview_target_path.as_deref(),
            Some("/tmp/root/nested/report.pdf")
        );
        assert!(state.preview_loading);
    }

    #[test]
    fn quick_look_toggle_opens_and_requests_preview() {
        let mut state = AppState::default();
        state.active_pane = 0;
        {
            let pane = state.pane_mut(0);
            pane.entries = vec![test_list_entry("/tmp/report.pdf", "report.pdf")];
            pane.sort_entries();
            pane.select_single(0);
        }

        let _ = update(&mut state, Message::ToggleQuickLook);

        assert!(state.quick_look_open);
        assert_eq!(
            state.preview_target_path.as_deref(),
            Some("/tmp/report.pdf")
        );
        assert!(state.preview_loading);
    }

    #[test]
    fn quick_look_close_clears_private_preview_state() {
        let mut state = AppState::default();
        state.quick_look_open = true;
        state.preview_target_path = Some("/tmp/report.pdf".to_string());
        state.preview_loading = true;

        let _ = update(&mut state, Message::CloseQuickLook);

        assert!(!state.quick_look_open);
        assert!(state.preview_target_path.is_none());
        assert!(!state.preview_loading);
    }

    #[test]
    fn quick_look_next_moves_single_selection_and_refreshes_preview() {
        let mut state = AppState::default();
        state.quick_look_open = true;
        state.keyboard_modifiers = iced::keyboard::Modifiers::SHIFT;
        {
            let pane = state.pane_mut(0);
            pane.entries = vec![
                test_list_entry("/tmp/alpha.txt", "alpha.txt"),
                test_list_entry("/tmp/bravo.txt", "bravo.txt"),
            ];
            pane.sort_entries();
            pane.select_single(0);
        }

        let _ = update(&mut state, Message::QuickLookSelectNext);

        let pane = state.pane(0);
        assert_eq!(pane.focused_index, Some(1));
        assert_eq!(pane.selected_indices, [1].into_iter().collect());
        assert_eq!(state.preview_target_path.as_deref(), Some("/tmp/bravo.txt"));
        assert!(state.preview_loading);
    }

    #[test]
    fn command_palette_input_resets_selection() {
        let mut state = AppState::default();
        state.command_palette_open = true;
        state.command_palette_selected = 3;

        let _ = update(
            &mut state,
            Message::CommandPaletteInputChanged("quick".to_string()),
        );

        assert_eq!(state.command_palette_query, "quick");
        assert_eq!(state.command_palette_selected, 0);
    }

    #[test]
    fn command_palette_execute_dispatches_selected_command_and_closes() {
        let mut state = AppState::default();
        state.command_palette_open = true;
        state.command_palette_query = "quick".to_string();

        let _ = update(&mut state, Message::ExecuteCommandPaletteSelection);

        assert!(!state.command_palette_open);
        assert!(state.command_palette_query.is_empty());
        assert!(state.quick_look_open);
    }

    #[test]
    fn edit_path_starts_path_bar_editing() {
        let mut state = AppState::default();
        state.active_pane = 0;
        state.pane_mut(0).current_path = "/tmp".to_string();

        let _ = update(&mut state, NavigationMessage::EditPath { pane: 0 }.into());

        let pane = state.pane(0);
        assert!(pane.path_editing);
        assert_eq!(pane.path_input, "/tmp");
    }

    #[test]
    fn edit_path_targets_requested_pane() {
        let mut state = AppState::default();
        state.active_pane = 0;
        state.pane_mut(0).path_editing = true;
        state.pane_mut(0).path_input = "/tmp/old".to_string();
        state.pane_mut(0).path_suggestions = vec![crate::state::PathSuggestion {
            label: "/tmp/old-child".to_string(),
            value: "/tmp/old-child".to_string(),
        }];
        state.pane_mut(0).path_suggestion_selected = Some(0);
        state.pane_mut(1).current_path = "/var".to_string();

        let _ = update(&mut state, NavigationMessage::EditPath { pane: 1 }.into());

        assert_eq!(state.active_pane, 1);
        assert!(!state.pane(0).path_editing);
        assert!(state.pane(0).path_input.is_empty());
        assert!(state.pane(0).path_suggestions.is_empty());
        assert_eq!(state.pane(0).path_suggestion_selected, None);
        assert!(state.pane(1).path_editing);
        assert_eq!(state.pane(1).path_input, "/var");
    }

    #[test]
    fn path_bar_commit_resolves_relative_path_through_navigation() {
        let mut state = AppState::default();
        state.active_pane = 0;
        {
            let pane = state.pane_mut(0);
            pane.current_path = "/tmp".to_string();
            pane.path_editing = true;
            pane.path_input = "iris-path-bar-child".to_string();
        }

        let _ = update(&mut state, Message::Pane(0, PaneMessage::CommitPathEdit));

        let pane = state.pane(0);
        assert!(!pane.path_editing);
        assert!(pane.path_input.is_empty());
        assert_eq!(pane.current_path, "/tmp/iris-path-bar-child");
    }

    #[test]
    fn path_bar_input_builds_directory_suggestions() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.current_path = "/tmp".to_string();
            pane.path_editing = true;
            pane.entries = vec![
                test_dir_entry("/tmp/iris-alpha", "iris-alpha"),
                test_dir_entry("/tmp/iris-beta", "iris-beta"),
                test_list_entry("/tmp/iris-note.txt", "iris-note.txt"),
            ];
        }

        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::PathInputChanged("iris".to_string())),
        );

        let pane = state.pane(0);
        let values = pane
            .path_suggestions
            .iter()
            .map(|suggestion| suggestion.value.as_str())
            .collect::<Vec<_>>();
        assert!(values.starts_with(&["iris-alpha", "iris-beta"]));
        assert!(!values.contains(&"iris-note.txt"));
        assert_eq!(pane.path_suggestion_selected, None);
    }

    #[test]
    fn path_bar_suggestion_selection_and_acceptance_updates_input() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.current_path = "/tmp".to_string();
            pane.path_editing = true;
            pane.entries = vec![
                test_dir_entry("/tmp/iris-alpha", "iris-alpha"),
                test_dir_entry("/tmp/iris-beta", "iris-beta"),
            ];
        }
        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::PathInputChanged("iris".to_string())),
        );
        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::PathSuggestionNext),
        );
        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::PathSuggestionNext),
        );
        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::AcceptPathSuggestion),
        );

        let pane = state.pane(0);
        assert_eq!(pane.path_input, "iris-beta");
        assert_eq!(
            pane.path_suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["iris-beta"]
        );
    }

    #[test]
    fn item_context_menu_opens_at_mouse_position_and_selects_target() {
        let mut state = AppState::default();
        state.active_pane = 0;
        state.mouse_position = Some(iced::Point::new(42.0, 84.0));
        {
            let pane = state.pane_mut(0);
            pane.entries = vec![
                test_list_entry("/tmp/alpha.txt", "alpha.txt"),
                test_list_entry("/tmp/beta.txt", "beta.txt"),
            ];
        }

        let _ = update(
            &mut state,
            Message::OpenItemContextMenu { pane: 0, index: 1 },
        );

        let menu = state.context_menu.as_ref().expect("context menu");
        assert_eq!(menu.target_name, "beta.txt");
        assert_eq!(menu.position, iced::Point::new(42.0, 84.0));
        assert!(matches!(
            menu.kind,
            crate::state::ContextMenuKind::Item { pane: 0, index: 1 }
        ));
        assert_eq!(state.pane(0).selected_indices, [1].into_iter().collect());
    }

    #[test]
    fn item_context_menu_rename_uses_menu_target_and_closes() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.entries = vec![
                test_list_entry("/tmp/alpha.txt", "alpha.txt"),
                test_list_entry("/tmp/beta.txt", "beta.txt"),
            ];
        }
        let _ = update(
            &mut state,
            Message::OpenItemContextMenu { pane: 0, index: 1 },
        );

        let _ = update(&mut state, Message::ItemActionRename);

        assert!(state.context_menu.is_none());
        assert_eq!(
            state
                .pane(0)
                .inline_rename
                .as_ref()
                .map(|rename| rename.original_name.as_str()),
            Some("beta.txt")
        );
    }

    #[test]
    fn sidebar_context_menu_opens_at_mouse_position_and_marks_removable() {
        let mut state = AppState::default();
        state.mouse_position = Some(iced::Point::new(12.0, 34.0));
        state.sidebar_groups = vec![SidebarGroup {
            name: "Favorites".to_string(),
            collapsed: false,
            kind: crate::state::SidebarGroupKind::Favorites,
            items: vec![SidebarItem {
                name: "Projects".to_string(),
                path: "/work/projects".to_string(),
                badge: "*".to_string(),
                badge_color: [0.2, 0.3, 0.4, 1.0],
                ejectable: false,
            }],
        }];

        let _ = update(
            &mut state,
            Message::OpenSidebarContextMenu { group: 0, item: 0 },
        );

        let menu = state.context_menu.as_ref().expect("context menu");
        assert_eq!(menu.target_name, "Projects");
        assert_eq!(menu.position, iced::Point::new(12.0, 34.0));
        assert!(matches!(
            menu.kind,
            crate::state::ContextMenuKind::SidebarItem {
                group: 0,
                item: 0,
                removable: true,
            }
        ));
    }

    #[test]
    fn sidebar_context_open_navigates_menu_target_and_closes() {
        let mut state = AppState::default();
        state.active_pane = 1;
        state.sidebar_groups = vec![SidebarGroup {
            name: "Places".to_string(),
            collapsed: false,
            kind: crate::state::SidebarGroupKind::Places,
            items: vec![SidebarItem {
                name: "Archive".to_string(),
                path: "/remote/archive".to_string(),
                badge: "A".to_string(),
                badge_color: [0.4, 0.5, 0.6, 1.0],
                ejectable: false,
            }],
        }];
        state.context_menu = Some(crate::state::ContextMenuState {
            kind: crate::state::ContextMenuKind::SidebarItem {
                group: 0,
                item: 0,
                removable: false,
            },
            target_name: "Archive".to_string(),
            position: iced::Point::ORIGIN,
        });

        let _ = update(&mut state, Message::SidebarContextOpen);

        assert!(state.context_menu.is_none());
        assert_eq!(state.pane(1).current_path, "/remote/archive");
    }

    #[test]
    fn tab_context_menu_records_available_actions_for_target_tab() {
        let mut state = AppState::default();
        state.mouse_position = Some(iced::Point::new(88.0, 16.0));
        state.pane_mut(0).current_path = "/tmp/alpha".to_string();
        state.open_tab(0);
        state.pane_mut(0).current_path = "/tmp/beta".to_string();
        state.open_tab(0);
        state.pane_mut(0).current_path = "/tmp/gamma".to_string();

        let _ = update(&mut state, Message::OpenTabContextMenu { pane: 0, tab: 1 });

        let menu = state.context_menu.as_ref().expect("context menu");
        assert_eq!(menu.target_name, "beta");
        assert_eq!(menu.position, iced::Point::new(88.0, 16.0));
        assert!(matches!(
            menu.kind,
            crate::state::ContextMenuKind::Tab {
                pane: 0,
                tab: 1,
                can_close: true,
                can_move_left: true,
                can_move_right: true,
            }
        ));
    }

    #[test]
    fn tab_context_action_closes_menu_before_forwarding() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/tmp/alpha".to_string();
        state.open_tab(0);
        state.pane_mut(0).current_path = "/tmp/beta".to_string();
        state.context_menu = Some(crate::state::ContextMenuState {
            kind: crate::state::ContextMenuKind::Tab {
                pane: 0,
                tab: 0,
                can_close: true,
                can_move_left: false,
                can_move_right: true,
            },
            target_name: "alpha".to_string(),
            position: iced::Point::ORIGIN,
        });

        let _ = update(
            &mut state,
            crate::messages::TabMessage::Duplicate { pane: 0, tab: 0 }.into(),
        );

        assert!(state.context_menu.is_none());
        assert_eq!(state.pane_tabs(0).tabs().len(), 3);
    }

    #[test]
    fn shift_focus_uses_target_pane_selection_anchor() {
        let mut state = AppState::default();
        state.keyboard_modifiers = iced::keyboard::Modifiers::SHIFT;
        {
            let pane = state.pane_mut(0);
            pane.entries = vec![
                test_list_entry("/left/alpha.txt", "alpha.txt"),
                test_list_entry("/left/bravo.txt", "bravo.txt"),
                test_list_entry("/left/charlie.txt", "charlie.txt"),
            ];
            pane.sort_entries();
            pane.select_single(2);
        }
        {
            let pane = state.pane_mut(1);
            pane.entries = vec![
                test_list_entry("/right/alpha.txt", "alpha.txt"),
                test_list_entry("/right/bravo.txt", "bravo.txt"),
                test_list_entry("/right/charlie.txt", "charlie.txt"),
            ];
            pane.sort_entries();
            pane.select_single(0);
        }

        let _ = update(&mut state, Message::Pane(1, PaneMessage::FocusNext));

        let pane = state.pane(1);
        assert_eq!(pane.focused_index, Some(1));
        assert_eq!(pane.selected_indices, [0, 1].into_iter().collect());
        assert_eq!(
            pane.selection_anchor_path.as_deref(),
            Some("/right/alpha.txt")
        );
    }

    #[test]
    fn cancel_recursive_filter_cancels_token_and_invalidates_run() {
        let mut state = AppState::default();
        let (run_id, token) = state.pane_mut(0).begin_recursive_filter();

        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::CancelRecursiveFilter),
        );

        let pane = state.pane(0);
        assert!(token.is_cancelled());
        assert_ne!(pane.filter_run_id, run_id);
        assert!(!pane.filter_recursive_running);
        assert!(pane.filter_cancellation_token.is_none());
    }

    #[test]
    fn shortcut_recording_updates_overrides_from_keypress() {
        let mut state = AppState::default();
        let _ = update(&mut state, SettingsMessage::Open.into());
        let _ = update(
            &mut state,
            SettingsMessage::BeginShortcutRecording("copy".to_string()).into(),
        );

        let _ = update(
            &mut state,
            Message::KeyPressed(
                iced::keyboard::Key::Character("y".into()),
                iced::keyboard::Modifiers::SHIFT,
            ),
        );

        assert!(state.shortcut_recording_action.is_none());
        assert_eq!(state.shortcut_overrides.len(), 1);
        assert_eq!(state.shortcut_overrides[0].action, "copy");
        assert_eq!(state.shortcut_overrides[0].combo, "Shift+Y");
    }

    #[test]
    fn locale_choice_updates_translations_immediately() {
        let mut state = AppState::default();

        let _ = update(
            &mut state,
            SettingsMessage::SetLocaleChoice(crate::config::LocaleChoice::Japanese).into(),
        );
        assert_ne!(crate::i18n::tr("settings-title"), "Settings");

        let _ = update(
            &mut state,
            SettingsMessage::SetLocaleChoice(crate::config::LocaleChoice::English).into(),
        );
        assert_eq!(crate::i18n::tr("settings-title"), "Settings");
    }

    #[test]
    fn map_tile_fetch_setting_updates_state_and_dialog() {
        let mut state = AppState::default();
        let _ = update(&mut state, SettingsMessage::Open.into());

        let _ = update(
            &mut state,
            SettingsMessage::SetMapTileFetchEnabled(true).into(),
        );

        assert!(state.map_tile_fetch_enabled);
        match &state.active_dialog {
            Some(crate::state::DialogState::Settings {
                map_tile_fetch_enabled,
                ..
            }) => assert!(*map_tile_fetch_enabled),
            _ => panic!("settings dialog should be open"),
        }
    }

    #[test]
    fn notification_timeout_setting_updates_state_and_dialog() {
        let mut state = AppState::default();
        let _ = update(&mut state, SettingsMessage::Open.into());

        let _ = update(
            &mut state,
            SettingsMessage::SetNotificationTimeoutInput("12".to_string()).into(),
        );

        assert_eq!(state.notification_timeout_secs, 12);
        match &state.active_dialog {
            Some(crate::state::DialogState::Settings {
                notification_timeout_input,
                ..
            }) => assert_eq!(notification_timeout_input, "12"),
            _ => panic!("settings dialog should be open"),
        }
    }

    #[test]
    fn invalid_notification_timeout_input_keeps_existing_setting() {
        let mut state = AppState::default();
        state.notification_timeout_secs = 7;
        let _ = update(&mut state, SettingsMessage::Open.into());

        let _ = update(
            &mut state,
            SettingsMessage::SetNotificationTimeoutInput("999".to_string()).into(),
        );

        assert_eq!(state.notification_timeout_secs, 7);
        match &state.active_dialog {
            Some(crate::state::DialogState::Settings {
                notification_timeout_input,
                ..
            }) => assert_eq!(notification_timeout_input, "999"),
            _ => panic!("settings dialog should be open"),
        }
    }

    #[test]
    fn smb_native_connection_fields_update_dialog_form() {
        let mut state = AppState::default();
        let _ = update(&mut state, SettingsMessage::Open.into());

        let _ = update(
            &mut state,
            crate::messages::ConnectionMessage::NetworkSmbPortChanged("1445".to_string()).into(),
        );
        let _ = update(
            &mut state,
            crate::messages::ConnectionMessage::NetworkSmbDomainChanged("WORKGROUP".to_string())
                .into(),
        );
        let _ = update(
            &mut state,
            crate::messages::ConnectionMessage::NetworkSmbPasswordChanged("secret".to_string())
                .into(),
        );

        match &state.active_dialog {
            Some(crate::state::DialogState::Settings { network_form, .. }) => {
                assert_eq!(network_form.smb_port_input, "1445");
                assert_eq!(network_form.smb_domain, "WORKGROUP");
                assert_eq!(network_form.smb_password, "secret");
            }
            _ => panic!("settings dialog should be open"),
        }
    }

    #[test]
    fn invalid_smb_port_blocks_network_connect() {
        let mut state = AppState::default();
        state.active_dialog = Some(crate::state::DialogState::ConnectNetwork {
            form: crate::state::NetworkConnectFormState {
                server: "nas.local".to_string(),
                share_path: "photo".to_string(),
                smb_port_input: "0".to_string(),
                ..crate::state::NetworkConnectFormState::default()
            },
        });

        let _ = update(
            &mut state,
            crate::messages::ConnectionMessage::PerformConnectNetwork.into(),
        );

        match &state.active_dialog {
            Some(crate::state::DialogState::ConnectNetwork { form }) => {
                assert!(form.error_message.is_some());
            }
            _ => panic!("network dialog should remain open"),
        }
    }

    #[test]
    fn incomplete_smb_credentials_block_network_connect() {
        let mut state = AppState::default();
        state.active_dialog = Some(crate::state::DialogState::ConnectNetwork {
            form: crate::state::NetworkConnectFormState {
                server: "nas.local".to_string(),
                share_path: "photo".to_string(),
                username: "iris".to_string(),
                ..crate::state::NetworkConnectFormState::default()
            },
        });

        let _ = update(
            &mut state,
            crate::messages::ConnectionMessage::PerformConnectNetwork.into(),
        );

        match &state.active_dialog {
            Some(crate::state::DialogState::ConnectNetwork { form }) => {
                assert!(form.error_message.is_some());
            }
            _ => panic!("network dialog should remain open"),
        }
    }

    #[test]
    fn gps_visible_tiles_do_not_fetch_when_setting_disabled() {
        let mut state = AppState::default();
        state.pane_mut(0).filter_gps_picker_open = true;

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::GpsMapVisibleTiles(vec![crate::views::filter::map_panel::TileCoord {
                    z: 8,
                    x: 227,
                    y: 100,
                }]),
            ),
        );

        assert!(state.pane(0).filter_map_tiles_loading.is_empty());
        assert!(state.pane(0).filter_map_tiles.is_empty());
    }

    #[test]
    fn gps_loaded_tile_is_stored_as_handle() {
        let mut state = AppState::default();
        let tile = crate::views::filter::map_panel::TileCoord {
            z: 8,
            x: 227,
            y: 100,
        };
        state.pane_mut(0).filter_map_tiles_loading.insert(tile);

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::GpsMapTileLoaded {
                    tile,
                    result: Ok(vec![137, 80, 78, 71]),
                },
            ),
        );

        assert!(!state.pane(0).filter_map_tiles_loading.contains(&tile));
        assert!(state.pane(0).filter_map_tiles.contains_key(&tile));
    }

    #[test]
    fn gps_failed_tile_is_not_rescheduled_immediately() {
        let mut state = AppState::default();
        let tile = crate::views::filter::map_panel::TileCoord {
            z: 8,
            x: 227,
            y: 100,
        };
        state.map_tile_fetch_enabled = true;
        state.pane_mut(0).filter_gps_picker_open = true;
        state.pane_mut(0).filter_map_tiles_loading.insert(tile);

        let _ = update(
            &mut state,
            Message::Pane(
                0,
                PaneMessage::GpsMapTileLoaded {
                    tile,
                    result: Err("network unavailable".to_string()),
                },
            ),
        );
        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::GpsMapVisibleTiles(vec![tile])),
        );

        assert!(!state.pane(0).filter_map_tiles_loading.contains(&tile));
        assert!(state.pane(0).filter_map_tiles_failed.contains_key(&tile));
    }

    #[test]
    fn gps_failed_tile_is_retried_after_backoff() {
        let mut state = AppState::default();
        let tile = crate::views::filter::map_panel::TileCoord {
            z: 8,
            x: 227,
            y: 100,
        };
        state.map_tile_fetch_enabled = true;
        state.pane_mut(0).filter_gps_picker_open = true;
        state.pane_mut(0).filter_map_tiles_failed.insert(
            tile,
            std::time::Instant::now()
                - crate::update::pane::MAP_TILE_RETRY_BACKOFF
                - std::time::Duration::from_secs(1),
        );

        let _ = update(
            &mut state,
            Message::Pane(0, PaneMessage::GpsMapVisibleTiles(vec![tile])),
        );

        assert!(state.pane(0).filter_map_tiles_loading.contains(&tile));
        assert!(!state.pane(0).filter_map_tiles_failed.contains_key(&tile));
    }

    fn test_list_entry(path: &str, name: &str) -> ListEntry {
        ListEntry {
            object_ref: ObjectRef {
                key: ObjectKey {
                    backend_id: BackendId("local".to_string()),
                    stable_id: StableId(path.to_string()),
                },
                parent_stable_id: None,
                display_path: path.to_string(),
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

    fn test_dir_entry(path: &str, name: &str) -> ListEntry {
        let mut entry = test_list_entry(path, name);
        entry.is_dir = true;
        entry
    }
}
