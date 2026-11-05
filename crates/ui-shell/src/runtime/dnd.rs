use crate::input::dnd::DndOperation;

#[cfg(target_os = "macos")]
pub(super) fn forced_dnd_operation(modifiers: iced::keyboard::Modifiers) -> Option<DndOperation> {
    if modifiers.alt() && modifiers.logo() {
        Some(DndOperation::Link)
    } else if modifiers.alt() {
        Some(DndOperation::Copy)
    } else if modifiers.logo() {
        Some(DndOperation::Move)
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
pub(super) fn forced_dnd_operation(modifiers: iced::keyboard::Modifiers) -> Option<DndOperation> {
    if modifiers.alt() || (modifiers.control() && modifiers.shift()) {
        Some(DndOperation::Link)
    } else if modifiers.control() {
        Some(DndOperation::Copy)
    } else if modifiers.shift() {
        Some(DndOperation::Move)
    } else {
        None
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
pub(super) fn forced_dnd_operation(modifiers: iced::keyboard::Modifiers) -> Option<DndOperation> {
    if modifiers.control() {
        Some(DndOperation::Copy)
    } else if modifiers.shift() {
        Some(DndOperation::Move)
    } else {
        None
    }
}
