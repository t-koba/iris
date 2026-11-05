use crate::state::{AppState, PreviewPosition};
use crate::views::theme;

pub const MIN_WINDOW_WIDTH: f32 = 640.0;
pub const MIN_WINDOW_HEIGHT: f32 = 480.0;
pub const MIN_SINGLE_PANE_WIDTH: f32 = 360.0;
pub const MIN_DUAL_PANE_WIDTH: f32 = 280.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneLayoutMetrics {
    pub width: f32,
    pub content_width: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedLayout {
    pub window_width: f32,
    pub window_height: f32,
    pub sidebar_inline: bool,
    pub sidebar_overlay: bool,
    pub right_panel_inline: bool,
    pub right_panel_overlay: bool,
    pub sidebar_width: f32,
    pub right_panel_width: f32,
    pub main_left: f32,
    pub main_width: f32,
    pub panes: [PaneLayoutMetrics; 2],
}

impl ResolvedLayout {
    pub fn resolve(state: &AppState) -> Self {
        let window_width = state.window_width.max(1.0);
        let window_height = state.window_height.max(1.0);
        let divider = if state.dual_pane_enabled {
            theme::RESIZE_HANDLE_WIDTH
        } else {
            0.0
        };
        let minimum_main = if state.dual_pane_enabled {
            MIN_DUAL_PANE_WIDTH * 2.0 + divider
        } else {
            MIN_SINGLE_PANE_WIDTH
        };

        let requested_sidebar = state.sidebar_visible;
        let sidebar_width = state
            .sidebar_width
            .clamp(theme::SIDEBAR_MIN_WIDTH, theme::SIDEBAR_MAX_WIDTH);
        let sidebar_span = sidebar_width + theme::RESIZE_HANDLE_WIDTH;
        let sidebar_inline = requested_sidebar && window_width >= minimum_main + sidebar_span;

        let requested_right = state.preview_panel_visible || state.properties_panel_visible;
        let horizontal_right = matches!(
            state.preview_position,
            PreviewPosition::Left | PreviewPosition::Right
        );
        let right_panel_width = state
            .right_panel_width
            .clamp(theme::RIGHT_PANEL_MIN_WIDTH, theme::RIGHT_PANEL_MAX_WIDTH);
        let right_span = right_panel_width + theme::RESIZE_HANDLE_WIDTH;
        let after_sidebar = window_width - if sidebar_inline { sidebar_span } else { 0.0 };
        let right_panel_inline =
            requested_right && (!horizontal_right || after_sidebar >= minimum_main + right_span);

        let consumed_sidebar = if sidebar_inline { sidebar_span } else { 0.0 };
        let consumed_right = if right_panel_inline && horizontal_right {
            right_span
        } else {
            0.0
        };
        let main_width = (window_width - consumed_sidebar - consumed_right).max(1.0);

        let pane_available = (main_width - divider).max(1.0);
        let (left_width, right_width) = if state.dual_pane_enabled {
            let minimum_ratio = (MIN_DUAL_PANE_WIDTH / pane_available).min(0.5);
            let ratio = state
                .pane_split_ratio
                .clamp(minimum_ratio, 1.0 - minimum_ratio);
            (pane_available * ratio, pane_available * (1.0 - ratio))
        } else {
            (pane_available, 0.0)
        };

        let main_left = consumed_sidebar
            + if right_panel_inline && state.preview_position == PreviewPosition::Left {
                right_span
            } else {
                0.0
            };

        Self {
            window_width,
            window_height,
            sidebar_inline,
            sidebar_overlay: requested_sidebar && !sidebar_inline,
            right_panel_inline,
            right_panel_overlay: requested_right && !right_panel_inline,
            sidebar_width,
            right_panel_width,
            main_left,
            main_width,
            panes: [
                PaneLayoutMetrics {
                    width: left_width,
                    content_width: (left_width - 16.0).max(1.0),
                },
                PaneLayoutMetrics {
                    width: right_width,
                    content_width: (right_width - 16.0).max(1.0),
                },
            ],
        }
    }

    pub fn pane(&self, index: usize) -> PaneLayoutMetrics {
        self.panes[index.min(1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_single_pane_preserves_main_content_and_overlays_auxiliary_panels() {
        let mut state = AppState::default();
        state.window_width = MIN_WINDOW_WIDTH;
        state.window_height = MIN_WINDOW_HEIGHT;
        state.sidebar_visible = true;
        state.preview_panel_visible = true;

        let layout = ResolvedLayout::resolve(&state);

        assert!(layout.pane(0).width >= MIN_SINGLE_PANE_WIDTH);
        assert!(layout.sidebar_inline);
        assert!(layout.right_panel_overlay);
        assert!(!layout.right_panel_inline);
    }

    #[test]
    fn narrow_dual_pane_keeps_both_panes_usable() {
        let mut state = AppState::default();
        state.window_width = MIN_WINDOW_WIDTH;
        state.dual_pane_enabled = true;
        state.sidebar_visible = true;
        state.preview_panel_visible = true;

        let layout = ResolvedLayout::resolve(&state);

        assert!(layout.pane(0).width >= MIN_DUAL_PANE_WIDTH);
        assert!(layout.pane(1).width >= MIN_DUAL_PANE_WIDTH);
        assert!(layout.sidebar_overlay);
        assert!(layout.right_panel_overlay);
    }

    #[test]
    fn wide_layout_accounts_for_sidebar_right_panel_and_split_ratio() {
        let mut state = AppState::default();
        state.window_width = 1_440.0;
        state.dual_pane_enabled = true;
        state.sidebar_visible = true;
        state.preview_panel_visible = true;
        state.pane_split_ratio = 0.6;

        let layout = ResolvedLayout::resolve(&state);

        assert!(layout.sidebar_inline);
        assert!(layout.right_panel_inline);
        assert!(layout.pane(0).width > layout.pane(1).width);
        assert!(layout.main_left >= layout.sidebar_width);
    }

    #[test]
    fn supported_viewports_never_produce_negative_or_unusable_panes() {
        for (width, height) in [
            (640.0, 480.0),
            (800.0, 600.0),
            (1_280.0, 800.0),
            (1_440.0, 900.0),
            (1_920.0, 1_080.0),
        ] {
            for dual in [false, true] {
                for preview_position in PreviewPosition::ALL {
                    for sidebar in [false, true] {
                        for auxiliary in [false, true] {
                            let mut state = AppState::default();
                            state.window_width = width;
                            state.window_height = height;
                            state.dual_pane_enabled = dual;
                            state.preview_position = preview_position;
                            state.sidebar_visible = sidebar;
                            state.preview_panel_visible = auxiliary;
                            state.properties_panel_visible = auxiliary;

                            let layout = ResolvedLayout::resolve(&state);

                            assert!(layout.main_width > 0.0);
                            assert!(layout.pane(0).width > 0.0);
                            if dual {
                                assert!(layout.pane(0).width >= MIN_DUAL_PANE_WIDTH);
                                assert!(layout.pane(1).width >= MIN_DUAL_PANE_WIDTH);
                            } else {
                                assert!(layout.pane(0).width >= MIN_SINGLE_PANE_WIDTH);
                            }
                        }
                    }
                }
            }
        }
    }
}
