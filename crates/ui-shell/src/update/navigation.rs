use super::*;
use crate::messages::NavigationMessage;

pub fn handle(state: &mut AppState, message: NavigationMessage) -> Task<Message> {
    match message {
        NavigationMessage::Back => {
            let pi = state.active_pane;
            if let Some(path) = state.pane_mut(pi).go_back() {
                if pi == 0 {
                    state.pane_mut(0).current_path = path.clone();
                    reset_directory_state(state);
                    state.pane_mut(0).scroll_y = 0.0;
                    refresh_breadcrumbs(state, 0);
                    return load_directory_progressively(path, state.show_hidden_files);
                }
                return Task::done(Message::Pane(1, PaneMessage::NavigateToPath(path)));
            }
        }
        NavigationMessage::Forward => {
            let pi = state.active_pane;
            if let Some(path) = state.pane_mut(pi).go_forward() {
                if pi == 0 {
                    state.pane_mut(0).current_path = path.clone();
                    reset_directory_state(state);
                    state.pane_mut(0).scroll_y = 0.0;
                    refresh_breadcrumbs(state, 0);
                    return load_directory_progressively(path, state.show_hidden_files);
                }
                return Task::done(Message::Pane(1, PaneMessage::NavigateToPath(path)));
            }
        }
        NavigationMessage::Up => {
            let pi = state.active_pane;
            let current = state.pane(pi).current_path.clone();
            if let Some(parent) = Path::new(&current).parent() {
                let parent_str = parent.to_string_lossy().to_string();
                if pi == 0 {
                    return navigate_to_path(state, parent_str);
                }
                return Task::done(Message::Pane(1, PaneMessage::NavigateToPath(parent_str)));
            }
        }
        NavigationMessage::EditPath { pane: target_pane } => {
            state.active_pane = target_pane;
            for pane_idx in 0..2 {
                let pane = state.pane_mut(pane_idx);
                pane.path_editing = false;
                pane.path_input.clear();
                pane.path_suggestions.clear();
                pane.path_suggestion_selected = None;
            }
            let input_value =
                app_runtime().format_display_path(&state.pane(target_pane).current_path);
            {
                let pane = state.pane_mut(target_pane);
                pane.path_editing = true;
                pane.path_input = input_value;
            }
            crate::update::pane::refresh_path_suggestions(state, target_pane);
            let input_id = crate::app::ids::path_bar_input_id(target_pane);
            return Task::batch([
                iced::widget::operation::focus(input_id.clone()),
                iced::widget::operation::select_all(input_id),
            ]);
        }
        NavigationMessage::CopyTextToClipboard(text) => {
            return clipboard::write(text);
        }
    }

    Task::none()
}
