use crate::state::EntryIndex;
use iced::Point;

/// Minimum pointer movement before a press becomes a drag.
pub const DRAG_THRESHOLD_PX: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DndTarget {
    /// The directory the pane is showing.
    PaneDirectory {
        pane: usize,
    },
    Entry {
        pane: usize,
        index: EntryIndex,
    },
}

#[derive(Clone, Debug, Default)]
pub enum DndState {
    #[default]
    Idle,
    /// Mouse is down; becomes `Dragging` once the threshold is exceeded.
    Pressed {
        source_pane: usize,
        source_indices: Vec<EntryIndex>,
        origin: Point,
        target: Option<DndTarget>,
    },
    Dragging {
        source_pane: usize,
        source_indices: Vec<EntryIndex>,
        target: Option<DndTarget>,
    },
    /// Drag coming from another app or window.
    ExternalDragging {
        source_paths: Vec<std::path::PathBuf>,
        target: Option<DndTarget>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DndOperation {
    /// Ctrl/Option held.
    Copy,
    /// Default.
    Move,
    /// Link / alias / shortcut.
    Link,
}

impl DndState {
    pub fn target(&self) -> Option<DndTarget> {
        match self {
            Self::Pressed { target, .. }
            | Self::Dragging { target, .. }
            | Self::ExternalDragging { target, .. } => *target,
            Self::Idle => None,
        }
    }

    pub fn set_target(&mut self, next_target: Option<DndTarget>) {
        match self {
            Self::Pressed { target, .. }
            | Self::Dragging { target, .. }
            | Self::ExternalDragging { target, .. } => *target = next_target,
            Self::Idle => {}
        }
    }
}
