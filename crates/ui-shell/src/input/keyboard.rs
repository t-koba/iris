use crate::config::ShortcutOverride;
use crate::input::dnd::DndState;
use crate::messages::{DialogMessage, Message, NavigationMessage, PaneMessage, TabMessage};
use crate::state::{AppState, ViewMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    SelectAll,
    Copy,
    Cut,
    Paste,
    Refresh,
    OpenSelected,
    OpenCommandPalette,
    ToggleQuickLook,
    CreateFolder,
    EditPath,
    ToggleHiddenFiles,
    Rename,
    Delete,
    Undo,
    Redo,
    Back,
    Forward,
    Up,
    NewTab,
    CloseTab,
    NextTab,
    FocusFilter,
    DuplicateTab,
}

impl ShortcutAction {
    fn from_id(id: &str) -> Option<Self> {
        match id {
            "select_all" => Some(Self::SelectAll),
            "copy" => Some(Self::Copy),
            "cut" => Some(Self::Cut),
            "paste" => Some(Self::Paste),
            "refresh" => Some(Self::Refresh),
            "open_selected" => Some(Self::OpenSelected),
            "open_command_palette" => Some(Self::OpenCommandPalette),
            "toggle_quick_look" => Some(Self::ToggleQuickLook),
            "create_folder" => Some(Self::CreateFolder),
            "edit_path" => Some(Self::EditPath),
            "toggle_hidden_files" => Some(Self::ToggleHiddenFiles),
            "rename" => Some(Self::Rename),
            "delete" => Some(Self::Delete),
            "undo" => Some(Self::Undo),
            "redo" => Some(Self::Redo),
            "back" => Some(Self::Back),
            "forward" => Some(Self::Forward),
            "up" => Some(Self::Up),
            "new_tab" => Some(Self::NewTab),
            "close_tab" => Some(Self::CloseTab),
            "next_tab" => Some(Self::NextTab),
            "focus_filter" => Some(Self::FocusFilter),
            "duplicate_tab" => Some(Self::DuplicateTab),
            _ => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::SelectAll => "select_all",
            Self::Copy => "copy",
            Self::Cut => "cut",
            Self::Paste => "paste",
            Self::Refresh => "refresh",
            Self::OpenSelected => "open_selected",
            Self::OpenCommandPalette => "open_command_palette",
            Self::ToggleQuickLook => "toggle_quick_look",
            Self::CreateFolder => "create_folder",
            Self::EditPath => "edit_path",
            Self::ToggleHiddenFiles => "toggle_hidden_files",
            Self::Rename => "rename",
            Self::Delete => "delete",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Back => "back",
            Self::Forward => "forward",
            Self::Up => "up",
            Self::NewTab => "new_tab",
            Self::CloseTab => "close_tab",
            Self::NextTab => "next_tab",
            Self::FocusFilter => "focus_filter",
            Self::DuplicateTab => "duplicate_tab",
        }
    }
}

pub struct ShortcutDescription {
    pub action: &'static str,
    pub combo: &'static str,
}

pub struct ShortcutAssignment {
    pub action: &'static str,
    pub combos: String,
}

pub fn default_shortcuts() -> &'static [ShortcutDescription] {
    &[
        ShortcutDescription {
            action: "select_all",
            combo: "CmdOrCtrl+A",
        },
        ShortcutDescription {
            action: "copy",
            combo: "CmdOrCtrl+C",
        },
        ShortcutDescription {
            action: "cut",
            combo: "CmdOrCtrl+X",
        },
        ShortcutDescription {
            action: "paste",
            combo: "CmdOrCtrl+V",
        },
        ShortcutDescription {
            action: "refresh",
            combo: "CmdOrCtrl+R / F5",
        },
        ShortcutDescription {
            action: "open_selected",
            combo: "Enter / CmdOrCtrl+O",
        },
        ShortcutDescription {
            action: "open_command_palette",
            combo: "CmdOrCtrl+P",
        },
        ShortcutDescription {
            action: "toggle_quick_look",
            combo: "Space",
        },
        ShortcutDescription {
            action: "create_folder",
            combo: CREATE_FOLDER_DISPLAY_COMBO,
        },
        ShortcutDescription {
            action: "edit_path",
            combo: EDIT_PATH_DISPLAY_COMBO,
        },
        ShortcutDescription {
            action: "toggle_hidden_files",
            combo: "CmdOrCtrl+Shift+.",
        },
        ShortcutDescription {
            action: "rename",
            combo: "F2",
        },
        ShortcutDescription {
            action: "delete",
            combo: "Delete",
        },
        ShortcutDescription {
            action: "undo",
            combo: "CmdOrCtrl+Z",
        },
        ShortcutDescription {
            action: "redo",
            combo: "CmdOrCtrl+Shift+Z",
        },
        ShortcutDescription {
            action: "back",
            combo: "CmdOrCtrl+[ / Backspace",
        },
        ShortcutDescription {
            action: "forward",
            combo: "CmdOrCtrl+]",
        },
        ShortcutDescription {
            action: "up",
            combo: "CmdOrCtrl+Backspace",
        },
        ShortcutDescription {
            action: "new_tab",
            combo: "CmdOrCtrl+T",
        },
        ShortcutDescription {
            action: "close_tab",
            combo: "CmdOrCtrl+W",
        },
        ShortcutDescription {
            action: "next_tab",
            combo: "Ctrl+Tab",
        },
        ShortcutDescription {
            action: "focus_filter",
            combo: "CmdOrCtrl+F",
        },
        ShortcutDescription {
            action: "duplicate_tab",
            combo: "CmdOrCtrl+Shift+T",
        },
    ]
}

#[cfg(target_os = "macos")]
const CREATE_FOLDER_DISPLAY_COMBO: &str = "Cmd+Shift+N";
#[cfg(not(target_os = "macos"))]
const CREATE_FOLDER_DISPLAY_COMBO: &str = "Ctrl+N";

#[cfg(target_os = "macos")]
const EDIT_PATH_DISPLAY_COMBO: &str = "Cmd+L";
#[cfg(not(target_os = "macos"))]
const EDIT_PATH_DISPLAY_COMBO: &str = "Ctrl+L";

pub fn create_folder_display_combo() -> &'static str {
    CREATE_FOLDER_DISPLAY_COMBO
}

pub fn edit_path_display_combo() -> &'static str {
    EDIT_PATH_DISPLAY_COMBO
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct KeyCombo {
    command: bool,
    control: bool,
    shift: bool,
    alt: bool,
    key: ComboKey,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ComboKey {
    Character(String),
    Named(&'static str),
}

#[derive(Clone, Debug)]
struct ResolvedShortcut {
    action: ShortcutAction,
    combo: KeyCombo,
}

pub fn validate_shortcut_overrides(overrides: &[ShortcutOverride]) -> Result<(), String> {
    resolve_shortcuts(overrides).map(|_| ())
}

pub fn shortcut_assignments(
    overrides: &[ShortcutOverride],
) -> Result<Vec<ShortcutAssignment>, String> {
    let shortcuts = resolve_shortcuts(overrides)?;
    let mut assignments = Vec::new();

    for description in default_shortcuts() {
        if assignments
            .iter()
            .any(|assignment: &ShortcutAssignment| assignment.action == description.action)
        {
            continue;
        }
        let Some(action) = ShortcutAction::from_id(description.action) else {
            continue;
        };
        let combos = shortcuts
            .iter()
            .filter(|shortcut| shortcut.action == action)
            .map(|shortcut| shortcut.combo.to_string())
            .collect::<Vec<_>>()
            .join(" / ");
        assignments.push(ShortcutAssignment {
            action: description.action,
            combos,
        });
    }

    Ok(assignments)
}

pub fn combo_string_from_event(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<String> {
    KeyCombo::from_event(key, modifiers).map(|combo| combo.to_string())
}

pub fn handle_key_event(
    key: iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
    state: &AppState,
) -> Option<Message> {
    use iced::keyboard::key::Named;

    let pi = state.active_pane;

    // Dialogs capture the keyboard.
    if state.active_dialog.is_some() {
        return match key {
            iced::keyboard::Key::Named(Named::Escape) => Some(DialogMessage::Close.into()),
            _ => None,
        };
    }

    if state.active_pane().inline_rename.is_some() {
        return match key {
            iced::keyboard::Key::Named(Named::Escape) => {
                Some(Message::Pane(pi, PaneMessage::CancelInlineRename))
            }
            _ => None,
        };
    }

    if state.active_pane().path_editing {
        return match key {
            iced::keyboard::Key::Named(Named::ArrowDown) => {
                Some(Message::Pane(pi, PaneMessage::PathSuggestionNext))
            }
            iced::keyboard::Key::Named(Named::ArrowUp) => {
                Some(Message::Pane(pi, PaneMessage::PathSuggestionPrevious))
            }
            iced::keyboard::Key::Named(Named::Tab) => {
                Some(Message::Pane(pi, PaneMessage::AcceptPathSuggestion))
            }
            iced::keyboard::Key::Named(Named::Escape) => {
                Some(Message::Pane(pi, PaneMessage::CancelPathEdit))
            }
            iced::keyboard::Key::Named(Named::Enter) => {
                Some(Message::Pane(pi, PaneMessage::CommitPathEdit))
            }
            _ => None,
        };
    }

    if state.context_menu.is_some() {
        return match key {
            iced::keyboard::Key::Named(Named::Escape) => Some(Message::CloseContextMenu),
            _ => None,
        };
    }

    if state.command_palette_open {
        return match key {
            iced::keyboard::Key::Named(Named::Escape) => Some(Message::CloseCommandPalette),
            iced::keyboard::Key::Named(Named::ArrowDown) => Some(Message::CommandPaletteMoveNext),
            iced::keyboard::Key::Named(Named::ArrowUp) => Some(Message::CommandPaletteMovePrevious),
            iced::keyboard::Key::Named(Named::Enter) => {
                Some(Message::ExecuteCommandPaletteSelection)
            }
            _ => None,
        };
    }

    if state.quick_look_open {
        return match key {
            iced::keyboard::Key::Named(Named::Escape | Named::Space) => {
                Some(Message::CloseQuickLook)
            }
            iced::keyboard::Key::Named(Named::ArrowRight) => Some(Message::QuickLookSelectNext),
            iced::keyboard::Key::Named(Named::ArrowLeft) => Some(Message::QuickLookSelectPrevious),
            _ => None,
        };
    }

    if let Some(message) = shortcut_message_for_event(&key, modifiers, state) {
        return Some(message);
    }

    match key {
        iced::keyboard::Key::Named(Named::ArrowDown) => {
            if state.active_pane().view_mode == ViewMode::Grid {
                Some(Message::Pane(pi, PaneMessage::FocusNextRow))
            } else {
                Some(Message::Pane(pi, PaneMessage::FocusNext))
            }
        }
        iced::keyboard::Key::Named(Named::ArrowUp) => {
            if state.active_pane().view_mode == ViewMode::Grid {
                Some(Message::Pane(pi, PaneMessage::FocusPreviousRow))
            } else {
                Some(Message::Pane(pi, PaneMessage::FocusPrevious))
            }
        }
        iced::keyboard::Key::Named(Named::ArrowRight) => {
            if state.active_pane().view_mode == ViewMode::Grid {
                Some(Message::Pane(pi, PaneMessage::FocusNext))
            } else {
                Some(Message::Pane(pi, PaneMessage::ActivateFocused))
            }
        }
        iced::keyboard::Key::Named(Named::ArrowLeft) => {
            if state.active_pane().view_mode == ViewMode::Grid {
                Some(Message::Pane(pi, PaneMessage::FocusPrevious))
            } else {
                Some(NavigationMessage::Up.into())
            }
        }
        iced::keyboard::Key::Named(Named::Escape) => {
            if !matches!(state.dnd_state, DndState::Idle) {
                Some(Message::DragEnd)
            } else if state.active_pane().is_filter_active() {
                Some(Message::ClearFilter)
            } else {
                Some(Message::Pane(pi, PaneMessage::ClearSelection))
            }
        }
        iced::keyboard::Key::Named(Named::Home) => state
            .active_pane()
            .first_visible_index()
            .map(|index| Message::Pane(pi, PaneMessage::SelectItem(index))),
        iced::keyboard::Key::Named(Named::End) => state
            .active_pane()
            .last_visible_index()
            .map(|index| Message::Pane(pi, PaneMessage::SelectItem(index))),
        _ => None,
    }
}

fn shortcut_message_for_event(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
    state: &AppState,
) -> Option<Message> {
    let event_combo = KeyCombo::from_event(key, modifiers)?;
    let shortcuts = resolve_shortcuts(&state.shortcut_overrides).ok()?;
    let action = shortcuts
        .iter()
        .find(|shortcut| shortcut.combo == event_combo)
        .map(|shortcut| shortcut.action)?;
    message_for_action(action, state)
}

fn message_for_action(action: ShortcutAction, state: &AppState) -> Option<Message> {
    let pi = state.active_pane;
    match action {
        ShortcutAction::SelectAll => Some(Message::Pane(pi, PaneMessage::SelectAll)),
        ShortcutAction::Copy => Some(Message::CopySelected),
        ShortcutAction::Cut => Some(Message::CutSelected),
        ShortcutAction::Paste => Some(Message::Paste),
        ShortcutAction::Refresh => Some(Message::Refresh),
        ShortcutAction::OpenSelected => Some(Message::OpenSelected),
        ShortcutAction::OpenCommandPalette => Some(Message::OpenCommandPalette),
        ShortcutAction::ToggleQuickLook => Some(Message::ToggleQuickLook),
        ShortcutAction::CreateFolder => Some(Message::CreateFolder),
        ShortcutAction::EditPath => Some(NavigationMessage::EditPath { pane: pi }.into()),
        ShortcutAction::ToggleHiddenFiles => Some(Message::ToggleHiddenFiles),
        ShortcutAction::Rename => state.active_pane().focused_index.map(Message::Rename),
        ShortcutAction::Delete => Some(Message::DeleteSelected),
        ShortcutAction::Undo => Some(Message::Undo),
        ShortcutAction::Redo => Some(Message::Redo),
        ShortcutAction::Back => Some(NavigationMessage::Back.into()),
        ShortcutAction::Forward => Some(NavigationMessage::Forward.into()),
        ShortcutAction::Up => Some(NavigationMessage::Up.into()),
        ShortcutAction::NewTab => Some(TabMessage::New { pane: pi }.into()),
        ShortcutAction::CloseTab => Some(
            TabMessage::Close {
                pane: pi,
                tab: state.pane_tabs(pi).active_index(),
            }
            .into(),
        ),
        ShortcutAction::NextTab => Some(TabMessage::SwitchNext { pane: pi }.into()),
        ShortcutAction::FocusFilter => Some(Message::FocusFilter),
        ShortcutAction::DuplicateTab => Some(
            TabMessage::Duplicate {
                pane: pi,
                tab: state.pane_tabs(pi).active_index(),
            }
            .into(),
        ),
    }
}

fn resolve_shortcuts(overrides: &[ShortcutOverride]) -> Result<Vec<ResolvedShortcut>, String> {
    let mut shortcuts = default_shortcut_specs()?;
    for override_entry in overrides {
        let action = ShortcutAction::from_id(&override_entry.action)
            .ok_or_else(|| format!("unknown shortcut action: {}", override_entry.action))?;
        let combo = KeyCombo::parse(&override_entry.combo)?;
        shortcuts.retain(|shortcut| shortcut.action != action);
        shortcuts.push(ResolvedShortcut { action, combo });
    }
    detect_shortcut_conflicts(&shortcuts)?;
    Ok(shortcuts)
}

fn default_shortcut_specs() -> Result<Vec<ResolvedShortcut>, String> {
    let mut specs = vec![
        (ShortcutAction::SelectAll, "CmdOrCtrl+A"),
        (ShortcutAction::Copy, "CmdOrCtrl+C"),
        (ShortcutAction::Cut, "CmdOrCtrl+X"),
        (ShortcutAction::Paste, "CmdOrCtrl+V"),
        (ShortcutAction::Refresh, "CmdOrCtrl+R"),
        (ShortcutAction::Refresh, "F5"),
        (ShortcutAction::OpenSelected, "Enter"),
        (ShortcutAction::OpenSelected, "CmdOrCtrl+O"),
        (ShortcutAction::OpenCommandPalette, "CmdOrCtrl+P"),
        (ShortcutAction::ToggleQuickLook, "Space"),
        (ShortcutAction::ToggleHiddenFiles, "CmdOrCtrl+Shift+."),
        (ShortcutAction::Rename, "F2"),
        (ShortcutAction::Delete, "Delete"),
        (ShortcutAction::Undo, "CmdOrCtrl+Z"),
        (ShortcutAction::Redo, "CmdOrCtrl+Shift+Z"),
        (ShortcutAction::Back, "CmdOrCtrl+["),
        (ShortcutAction::Back, "Backspace"),
        (ShortcutAction::Forward, "CmdOrCtrl+]"),
        (ShortcutAction::Up, "CmdOrCtrl+Backspace"),
        (ShortcutAction::NewTab, "CmdOrCtrl+T"),
        (ShortcutAction::CloseTab, "CmdOrCtrl+W"),
        (ShortcutAction::NextTab, "Ctrl+Tab"),
        (ShortcutAction::FocusFilter, "CmdOrCtrl+F"),
        (ShortcutAction::DuplicateTab, "CmdOrCtrl+Shift+T"),
    ];

    #[cfg(target_os = "macos")]
    {
        specs.push((ShortcutAction::CreateFolder, "CmdOrCtrl+Shift+N"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        specs.push((ShortcutAction::CreateFolder, "CmdOrCtrl+N"));
    }
    specs.push((ShortcutAction::EditPath, "CmdOrCtrl+L"));

    specs
        .into_iter()
        .map(|(action, combo)| {
            Ok(ResolvedShortcut {
                action,
                combo: KeyCombo::parse(combo)?,
            })
        })
        .collect()
}

fn detect_shortcut_conflicts(shortcuts: &[ResolvedShortcut]) -> Result<(), String> {
    for (index, shortcut) in shortcuts.iter().enumerate() {
        if let Some(conflict) = shortcuts
            .iter()
            .skip(index + 1)
            .find(|candidate| candidate.combo == shortcut.combo)
        {
            return Err(format!(
                "shortcut conflict: {} and {} both use {}",
                shortcut.action.id(),
                conflict.action.id(),
                shortcut.combo
            ));
        }
    }
    Ok(())
}

impl KeyCombo {
    fn parse(input: &str) -> Result<Self, String> {
        let mut command = false;
        let mut control = false;
        let mut shift = false;
        let mut alt = false;
        let mut key = None;

        for part in input.split('+') {
            let token = part.trim();
            if token.is_empty() {
                continue;
            }
            match token.to_ascii_lowercase().as_str() {
                "cmdorctrl" | "command" | "cmd" => command = true,
                "ctrl" | "control" => {
                    #[cfg(target_os = "macos")]
                    {
                        control = true;
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        command = true;
                    }
                }
                "shift" => shift = true,
                "alt" | "option" => alt = true,
                value => {
                    if key.is_some() {
                        return Err(format!("shortcut has multiple keys: {input}"));
                    }
                    key = Some(ComboKey::parse(value)?);
                }
            }
        }

        let Some(key) = key else {
            return Err(format!("shortcut is missing key: {input}"));
        };

        Ok(Self {
            command,
            control,
            shift,
            alt,
            key,
        })
    }

    fn from_event(key: &iced::keyboard::Key, modifiers: iced::keyboard::Modifiers) -> Option<Self> {
        let key = match key {
            iced::keyboard::Key::Character(value) => {
                ComboKey::Character(value.as_str().to_ascii_lowercase())
            }
            iced::keyboard::Key::Named(named) => ComboKey::from_named(*named)?,
            _ => return None,
        };

        Some(Self {
            command: modifiers.command(),
            control: modifiers.control() && !modifiers.command(),
            shift: modifiers.shift(),
            alt: modifiers.alt(),
            key,
        })
    }
}

impl ComboKey {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "enter" | "return" => Ok(Self::Named("enter")),
            "space" => Ok(Self::Named("space")),
            "backspace" => Ok(Self::Named("backspace")),
            "delete" => Ok(Self::Named("delete")),
            "tab" => Ok(Self::Named("tab")),
            "f2" => Ok(Self::Named("f2")),
            "f5" => Ok(Self::Named("f5")),
            value if value.chars().count() == 1 => Ok(Self::Character(value.to_string())),
            _ => Err(format!("unsupported shortcut key: {value}")),
        }
    }

    fn from_named(named: iced::keyboard::key::Named) -> Option<Self> {
        use iced::keyboard::key::Named;
        match named {
            Named::Enter => Some(Self::Named("enter")),
            Named::Space => Some(Self::Named("space")),
            Named::Backspace => Some(Self::Named("backspace")),
            Named::Delete => Some(Self::Named("delete")),
            Named::Tab => Some(Self::Named("tab")),
            Named::F2 => Some(Self::Named("f2")),
            Named::F5 => Some(Self::Named("f5")),
            _ => None,
        }
    }
}

impl std::fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.command {
            parts.push("CmdOrCtrl".to_string());
        }
        if self.control {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        parts.push(match &self.key {
            ComboKey::Character(value) => value.to_ascii_uppercase(),
            ComboKey::Named(value) => value.to_string(),
        });
        write!(f, "{}", parts.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::list::ListEntry;
    use iris_core_domain::types::metadata::MetadataLevel;
    use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};

    fn list_entry(name: &str) -> ListEntry {
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

    #[test]
    fn override_changes_existing_shortcut() {
        let overrides = [ShortcutOverride {
            action: "copy".to_string(),
            combo: "CmdOrCtrl+Shift+C".to_string(),
        }];
        let shortcuts = resolve_shortcuts(&overrides).unwrap();
        let copy = shortcuts
            .iter()
            .find(|shortcut| shortcut.action == ShortcutAction::Copy)
            .unwrap();
        assert_eq!(copy.combo, KeyCombo::parse("CmdOrCtrl+Shift+C").unwrap());
    }

    #[test]
    fn shortcut_assignments_show_effective_overrides() {
        let overrides = [ShortcutOverride {
            action: "copy".to_string(),
            combo: "CmdOrCtrl+Shift+C".to_string(),
        }];
        let assignments = shortcut_assignments(&overrides).unwrap();
        let copy = assignments
            .iter()
            .find(|assignment| assignment.action == "copy")
            .unwrap();
        assert_eq!(copy.combos, "CmdOrCtrl+Shift+C");
    }

    #[test]
    fn space_toggles_quick_look_instead_of_opening_selection() {
        let state = AppState::default();

        let message = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Space),
            iced::keyboard::Modifiers::default(),
            &state,
        );

        assert!(matches!(message, Some(Message::ToggleQuickLook)));
    }

    #[test]
    fn command_shortcut_opens_command_palette() {
        let state = AppState::default();

        let message = handle_key_event(
            iced::keyboard::Key::Character("p".into()),
            iced::keyboard::Modifiers::COMMAND,
            &state,
        );

        assert!(matches!(message, Some(Message::OpenCommandPalette)));
    }

    #[test]
    fn command_palette_keyboard_controls_are_consumed_by_overlay() {
        let mut state = AppState::default();
        state.command_palette_open = true;

        let close = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            iced::keyboard::Modifiers::default(),
            &state,
        );
        let next = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowDown),
            iced::keyboard::Modifiers::default(),
            &state,
        );
        let execute = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
            iced::keyboard::Modifiers::default(),
            &state,
        );

        assert!(matches!(close, Some(Message::CloseCommandPalette)));
        assert!(matches!(next, Some(Message::CommandPaletteMoveNext)));
        assert!(matches!(
            execute,
            Some(Message::ExecuteCommandPaletteSelection)
        ));
    }

    #[test]
    fn path_bar_keyboard_controls_are_consumed_by_editor() {
        let mut state = AppState::default();
        state.active_pane_mut().path_editing = true;

        let cancel = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            iced::keyboard::Modifiers::default(),
            &state,
        );
        let commit = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
            iced::keyboard::Modifiers::default(),
            &state,
        );
        let next = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowDown),
            iced::keyboard::Modifiers::default(),
            &state,
        );
        let previous = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowUp),
            iced::keyboard::Modifiers::default(),
            &state,
        );
        let accept = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
            iced::keyboard::Modifiers::default(),
            &state,
        );

        assert!(matches!(
            cancel,
            Some(Message::Pane(0, PaneMessage::CancelPathEdit))
        ));
        assert!(matches!(
            commit,
            Some(Message::Pane(0, PaneMessage::CommitPathEdit))
        ));
        assert!(matches!(
            next,
            Some(Message::Pane(0, PaneMessage::PathSuggestionNext))
        ));
        assert!(matches!(
            previous,
            Some(Message::Pane(0, PaneMessage::PathSuggestionPrevious))
        ));
        assert!(matches!(
            accept,
            Some(Message::Pane(0, PaneMessage::AcceptPathSuggestion))
        ));
    }

    #[test]
    fn escape_closes_context_menu() {
        let mut state = AppState::default();
        state.context_menu = Some(crate::state::ContextMenuState {
            kind: crate::state::ContextMenuKind::Item { pane: 0, index: 0 },
            target_name: "alpha.txt".to_string(),
            position: iced::Point::new(12.0, 24.0),
        });

        let close = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            iced::keyboard::Modifiers::default(),
            &state,
        );

        assert!(matches!(close, Some(Message::CloseContextMenu)));
    }

    #[test]
    fn quick_look_keyboard_controls_are_consumed_by_overlay() {
        let mut state = AppState::default();
        state.quick_look_open = true;

        let close = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            iced::keyboard::Modifiers::default(),
            &state,
        );
        let next = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowRight),
            iced::keyboard::Modifiers::SHIFT,
            &state,
        );
        let previous = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowLeft),
            iced::keyboard::Modifiers::COMMAND,
            &state,
        );

        assert!(matches!(close, Some(Message::CloseQuickLook)));
        assert!(matches!(next, Some(Message::QuickLookSelectNext)));
        assert!(matches!(previous, Some(Message::QuickLookSelectPrevious)));
    }

    #[test]
    fn override_rejects_unknown_action() {
        let overrides = [ShortcutOverride {
            action: "print".to_string(),
            combo: "CmdOrCtrl+P".to_string(),
        }];
        assert!(validate_shortcut_overrides(&overrides).is_err());
    }

    #[test]
    fn override_rejects_conflicting_combo() {
        let overrides = [ShortcutOverride {
            action: "copy".to_string(),
            combo: "CmdOrCtrl+X".to_string(),
        }];
        assert!(validate_shortcut_overrides(&overrides).is_err());
    }

    #[test]
    fn home_and_end_follow_visible_projection_order() {
        let mut state = AppState::default();
        state.pane_mut(0).entries = vec![list_entry("zeta.txt"), list_entry("alpha.txt")];
        state.pane_mut(0).sort_entries();

        let home = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Home),
            iced::keyboard::Modifiers::default(),
            &state,
        );
        let end = handle_key_event(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::End),
            iced::keyboard::Modifiers::default(),
            &state,
        );

        assert!(matches!(
            home,
            Some(Message::Pane(0, PaneMessage::SelectItem(1)))
        ));
        assert!(matches!(
            end,
            Some(Message::Pane(0, PaneMessage::SelectItem(0)))
        ));
    }
}
