use crate::i18n::tr;
use crate::messages::{Message, NavigationMessage, SettingsMessage, SidebarMessage, TabMessage};
use crate::state::AppState;

pub const COMMAND_PALETTE_VISIBLE_LIMIT: usize = 8;

#[derive(Clone, Debug)]
pub struct CommandPaletteItem {
    pub id: &'static str,
    pub label: String,
    pub shortcut: Option<&'static str>,
    pub message: Message,
}

/// Shared enablement rules for every surface that can dispatch file actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionAvailability {
    pub has_selection: bool,
    pub has_single_selection: bool,
    pub has_multiple_selection: bool,
    pub can_paste: bool,
    pub can_mutate: bool,
    pub can_create_folder: bool,
}

impl ActionAvailability {
    pub fn for_state(state: &AppState) -> Self {
        let pane = state.active_pane();
        let selected = pane.selected_indices.len();
        let can_mutate = state.pending_operation.is_none() && state.active_dialog.is_none();
        Self {
            has_selection: selected > 0,
            has_single_selection: selected == 1,
            has_multiple_selection: selected > 1,
            can_paste: state.clipboard.is_some() && can_mutate,
            can_mutate,
            can_create_folder: can_mutate && !pane.current_path.trim().is_empty(),
        }
    }
}

struct CommandSpec {
    id: &'static str,
    label_key: &'static str,
    keywords: &'static str,
    shortcut: Option<&'static str>,
    message: Message,
}

pub fn command_palette_results(state: &AppState) -> Vec<CommandPaletteItem> {
    let query = state.command_palette_query.trim();
    let commands = command_specs(state);

    if query.is_empty() {
        return commands.into_iter().map(CommandPaletteItem::from).collect();
    }

    let mut scored = commands
        .into_iter()
        .filter_map(|command| {
            let label = tr(command.label_key);
            let candidate = format!("{label} {} {}", command.id, command.keywords);
            iris_search::filter::fuzzy_name_score(&candidate, query)
                .map(|score| (score, label, command))
        })
        .collect::<Vec<_>>();

    scored.sort_by(
        |(left_score, left_label, _), (right_score, right_label, _)| {
            right_score
                .cmp(left_score)
                .then_with(|| left_label.cmp(right_label))
        },
    );

    scored
        .into_iter()
        .map(|(_, label, command)| CommandPaletteItem {
            id: command.id,
            label,
            shortcut: command.shortcut,
            message: command.message,
        })
        .collect()
}

fn command_specs(state: &AppState) -> Vec<CommandSpec> {
    let pane_idx = state.active_pane;
    let pane = state.active_pane();
    let active_tab = state.pane_tabs(pane_idx).active_index();
    let availability = ActionAvailability::for_state(state);
    let has_selection = availability.has_selection;
    let has_multiple_selection = availability.has_multiple_selection;

    let mut commands = Vec::new();
    if has_selection {
        commands.push(command(
            "open_selected",
            "command-open-selected",
            "open selected item file folder",
            Some("Enter / CmdOrCtrl+O"),
            Message::OpenSelected,
        ));
        commands.push(command(
            "copy_selected",
            "command-copy-selected",
            "copy selected items clipboard",
            Some("CmdOrCtrl+C"),
            Message::CopySelected,
        ));
        if availability.can_mutate {
            commands.push(command(
                "cut_selected",
                "command-cut-selected",
                "cut selected items clipboard",
                Some("CmdOrCtrl+X"),
                Message::CutSelected,
            ));
            commands.push(command(
                "delete_selected",
                "command-delete-selected",
                "delete selected items trash",
                Some("Delete"),
                Message::DeleteSelected,
            ));
        }
    }
    if availability.can_mutate {
        if let Some(index) = pane.focused_index {
            commands.push(command(
                "rename",
                "command-rename",
                "rename focused item",
                Some("F2"),
                Message::Rename(index),
            ));
        }
    }
    if has_multiple_selection && availability.can_mutate {
        commands.push(command(
            "batch_rename",
            "command-batch-rename",
            "batch rename selected items sequence replace regex date",
            None,
            Message::BatchRenameSelected,
        ));
    }
    if availability.can_paste {
        commands.push(command(
            "paste",
            "command-paste",
            "paste clipboard",
            Some("CmdOrCtrl+V"),
            Message::Paste,
        ));
    }
    if state.undo_journal.can_undo() {
        commands.push(command(
            "undo",
            "command-undo",
            "undo last operation",
            Some("CmdOrCtrl+Z"),
            Message::Undo,
        ));
    }
    if state.undo_journal.can_redo() {
        commands.push(command(
            "redo",
            "command-redo",
            "redo operation",
            Some("CmdOrCtrl+Shift+Z"),
            Message::Redo,
        ));
    }

    if has_selection && state.dual_pane_enabled && availability.can_mutate {
        commands.push(command(
            "copy_to_other_pane",
            "command-copy-to-other-pane",
            "copy selected to other pane",
            None,
            Message::CopyToOtherPane,
        ));
        commands.push(command(
            "move_to_other_pane",
            "command-move-to-other-pane",
            "move selected to other pane",
            None,
            Message::MoveToOtherPane,
        ));
    }

    commands.extend([
        command(
            "quick_look",
            "command-quick-look",
            "quick look preview space",
            Some("Space"),
            Message::ToggleQuickLook,
        ),
        command(
            "edit_path",
            "command-edit-path",
            "edit path location go",
            Some(crate::input::keyboard::edit_path_display_combo()),
            NavigationMessage::EditPath {
                pane: state.active_pane,
            }
            .into(),
        ),
        command(
            "refresh",
            "command-refresh",
            "reload refresh",
            Some("CmdOrCtrl+R / F5"),
            Message::Refresh,
        ),
        command(
            "toggle_preview_panel",
            "command-toggle-preview-panel",
            "preview panel show hide",
            None,
            Message::TogglePreviewPanel,
        ),
        command(
            "toggle_properties_panel",
            "command-toggle-properties-panel",
            "properties inspector panel show hide",
            None,
            Message::TogglePropertiesPanel,
        ),
        command(
            "toggle_sidebar",
            "command-toggle-sidebar",
            "sidebar show hide",
            None,
            SidebarMessage::ToggleVisibility.into(),
        ),
        command(
            "toggle_hidden_files",
            "command-toggle-hidden-files",
            "hidden files dotfiles show hide",
            Some("CmdOrCtrl+Shift+."),
            Message::ToggleHiddenFiles,
        ),
        command(
            "toggle_dual_pane",
            "command-toggle-dual-pane",
            "dual pane split",
            None,
            Message::ToggleDualPane,
        ),
        command(
            "back",
            "command-back",
            "back navigation history",
            Some("CmdOrCtrl+[ / Backspace"),
            NavigationMessage::Back.into(),
        ),
        command(
            "forward",
            "command-forward",
            "forward navigation history",
            Some("CmdOrCtrl+]"),
            NavigationMessage::Forward.into(),
        ),
        command(
            "up",
            "command-up",
            "parent folder up",
            Some("CmdOrCtrl+Backspace"),
            NavigationMessage::Up.into(),
        ),
        command(
            "new_tab",
            "command-new-tab",
            "new tab",
            Some("CmdOrCtrl+T"),
            TabMessage::New { pane: pane_idx }.into(),
        ),
        command(
            "duplicate_tab",
            "command-duplicate-tab",
            "duplicate tab",
            Some("CmdOrCtrl+Shift+T"),
            TabMessage::Duplicate {
                pane: pane_idx,
                tab: active_tab,
            }
            .into(),
        ),
        command(
            "close_tab",
            "command-close-tab",
            "close tab",
            Some("CmdOrCtrl+W"),
            TabMessage::Close {
                pane: pane_idx,
                tab: active_tab,
            }
            .into(),
        ),
        command(
            "next_tab",
            "command-next-tab",
            "next tab switch",
            Some("Ctrl+Tab"),
            TabMessage::SwitchNext { pane: pane_idx }.into(),
        ),
        command(
            "focus_filter",
            "command-focus-filter",
            "filter search focus",
            Some("CmdOrCtrl+F"),
            Message::FocusFilter,
        ),
        command(
            "settings",
            "command-settings",
            "settings preferences",
            None,
            SettingsMessage::Open.into(),
        ),
    ]);
    if availability.can_create_folder {
        commands.push(command(
            "create_folder",
            "command-create-folder",
            "new folder directory",
            Some(crate::input::keyboard::create_folder_display_combo()),
            Message::CreateFolder,
        ));
    }

    commands
}

fn command(
    id: &'static str,
    label_key: &'static str,
    keywords: &'static str,
    shortcut: Option<&'static str>,
    message: Message,
) -> CommandSpec {
    CommandSpec {
        id,
        label_key,
        keywords,
        shortcut,
        message,
    }
}

impl From<CommandSpec> for CommandPaletteItem {
    fn from(command: CommandSpec) -> Self {
        Self {
            id: command.id,
            label: tr(command.label_key),
            shortcut: command.shortcut,
            message: command.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::local_entry;

    #[test]
    fn command_palette_filters_by_fuzzy_query() {
        let mut state = AppState::default();
        state.command_palette_query = "quick".to_string();

        let results = command_palette_results(&state);

        assert_eq!(results.first().map(|item| item.id), Some("quick_look"));
    }

    #[test]
    fn command_palette_exposes_selection_commands_when_available() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.entries = vec![local_entry("/tmp/report.pdf", false)];
            pane.sort_entries();
            pane.select_single(0);
        }

        let ids = command_palette_results(&state)
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();

        assert!(ids.contains(&"open_selected"));
        assert!(ids.contains(&"rename"));
        assert!(ids.contains(&"delete_selected"));
    }

    #[test]
    fn command_palette_exposes_batch_rename_for_multiple_selection() {
        let mut state = AppState::default();
        {
            let pane = state.pane_mut(0);
            pane.entries = vec![
                local_entry("/tmp/a.txt", false),
                local_entry("/tmp/b.txt", false),
            ];
            pane.sort_entries();
            pane.select_single(0);
            pane.toggle_selection(1);
        }

        let ids = command_palette_results(&state)
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();

        assert!(ids.contains(&"batch_rename"));
    }

    #[test]
    fn action_availability_blocks_mutations_while_a_dialog_is_active() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/tmp".to_string();
        state.active_dialog = Some(crate::state::DialogState::Error {
            title: "Busy".to_string(),
            message: "Busy".to_string(),
        });

        let availability = ActionAvailability::for_state(&state);

        assert!(!availability.can_mutate);
        assert!(!availability.can_create_folder);
        assert!(!availability.can_paste);
    }
}
