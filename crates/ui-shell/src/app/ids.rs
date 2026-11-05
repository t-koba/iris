use iced::widget::Id;

pub fn pane_scroll_id(pane_idx: usize) -> Id {
    Id::from(format!("pane-{pane_idx}-virtual-scroll"))
}

pub fn filter_input_id(pane_idx: usize) -> Id {
    Id::from(format!("pane-{pane_idx}-filter-input"))
}

pub fn path_bar_input_id(pane_idx: usize) -> Id {
    Id::from(format!("pane-{pane_idx}-path-bar-input"))
}

pub fn inline_rename_input_id(pane_idx: usize, display_path: &str) -> Id {
    Id::from(format!(
        "pane-{pane_idx}-rename-{}",
        crate::observability::stable_hash(display_path.as_bytes())
    ))
}

pub fn command_palette_input_id() -> Id {
    Id::new("command-palette-input")
}
