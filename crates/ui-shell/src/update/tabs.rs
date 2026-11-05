use crate::messages::{Message, TabMessage};
use crate::state::{AppState, TabDragState};
use crate::update::helpers::{
    load_directory_progressively, load_secondary_directory_task, refresh_breadcrumbs_for_pane,
};
use crate::update::preview::refresh_preview_for_selection;
use iced::Task;

pub fn handle(state: &mut AppState, message: TabMessage) -> Task<Message> {
    match message {
        TabMessage::New { pane } => {
            if state.pane_checked(pane).is_none() {
                return Task::none();
            }
            state.active_pane = pane;
            state.open_tab(pane);
            reload_active_tab(state, pane)
        }
        TabMessage::Duplicate { pane, tab } => {
            if state.pane_checked(pane).is_none() {
                return Task::none();
            }
            state.active_pane = pane;
            let _ = state.switch_tab(pane, tab);
            state.open_tab(pane);
            reload_active_tab(state, pane)
        }
        TabMessage::Close { pane, tab } => {
            if state.pane_checked(pane).is_none() {
                return Task::none();
            }
            let was_active = state.pane_tabs(pane).active_index() == tab;
            if !state.close_tab(pane, tab) {
                return Task::none();
            }
            if was_active {
                state.active_pane = pane;
                reload_active_tab(state, pane)
            } else {
                Task::none()
            }
        }
        TabMessage::Switch { pane, tab } => {
            if state.pane_checked(pane).is_none() {
                return Task::none();
            }
            state.active_pane = pane;
            if state.switch_tab(pane, tab) {
                reload_active_tab(state, pane)
            } else {
                refresh_preview_for_selection(state)
            }
        }
        TabMessage::SwitchNext { pane } => {
            if state.pane_checked(pane).is_none() {
                return Task::none();
            }
            state.active_pane = pane;
            if state.switch_to_next_tab(pane) {
                reload_active_tab(state, pane)
            } else {
                Task::none()
            }
        }
        TabMessage::MoveLeft { pane, tab } => {
            if state.pane_checked(pane).is_none() || tab == 0 {
                return Task::none();
            }
            state.move_tab(pane, tab, tab - 1);
            Task::none()
        }
        TabMessage::MoveRight { pane, tab } => {
            if state.pane_checked(pane).is_none() {
                return Task::none();
            }
            let tab_count = state.pane_tabs(pane).tabs().len();
            if tab + 1 >= tab_count {
                return Task::none();
            }
            state.move_tab(pane, tab, tab + 1);
            Task::none()
        }
        TabMessage::BeginDrag { pane, tab } => {
            if state.pane_checked(pane).is_none() || tab >= state.pane_tabs(pane).tabs().len() {
                return Task::none();
            }
            state.tab_drag_state = Some(TabDragState {
                pane,
                from: tab,
                target: tab,
            });
            Task::none()
        }
        TabMessage::DragOver { pane, tab } => {
            if state.pane_checked(pane).is_none() {
                return Task::none();
            }
            let tab_count = state.pane_tabs(pane).tabs().len();
            let Some(drag) = state.tab_drag_state.as_mut() else {
                return Task::none();
            };
            if drag.pane == pane && tab < tab_count {
                drag.target = tab;
            }
            Task::none()
        }
        TabMessage::Drop => finish_tab_drag(state),
    }
}

pub fn finish_tab_drag(state: &mut AppState) -> Task<Message> {
    let Some(drag) = state.tab_drag_state.take() else {
        return Task::none();
    };
    if state.pane_checked(drag.pane).is_none() {
        return Task::none();
    }
    if state.move_tab(drag.pane, drag.from, drag.target) {
        Task::done(Message::SaveLayoutConfig)
    } else {
        Task::none()
    }
}

fn reload_active_tab(state: &mut AppState, pane: usize) -> Task<Message> {
    refresh_breadcrumbs_for_pane(state, pane);
    let preview_task = if state.active_pane == pane {
        refresh_preview_for_selection(state)
    } else {
        Task::none()
    };

    let path = state.pane(pane).current_path.clone();
    if path.is_empty() {
        return preview_task;
    }

    let load_task = if pane == 0 {
        load_directory_progressively(path, state.show_hidden_files)
    } else {
        load_secondary_directory_task(path)
    };
    Task::batch([preview_task, load_task])
}
