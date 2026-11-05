use super::navigation::{entry_offset, EntryIndex, PaneState};
use iris_core_domain::types::object::ObjectRef;

impl PaneState {
    pub fn toggle_selection(&mut self, index: EntryIndex) {
        if self.selected_indices.contains(&index) {
            self.selected_indices.remove(&index);
        } else {
            self.selected_indices.insert(index);
        }
        self.focused_index = Some(index);
        self.set_selection_anchor(index);
    }

    pub fn select_all(&mut self) {
        self.selected_indices = self.visible_indices().iter().copied().collect();
        self.focused_index = self.first_visible_index();
        self.selection_anchor_path = self
            .focused_index
            .and_then(|index| self.entry_display_path(index));
    }

    pub fn clear_selection(&mut self) {
        self.selected_indices.clear();
        self.selection_anchor_path = None;
    }

    pub fn select_single(&mut self, index: EntryIndex) {
        self.selected_indices.clear();
        self.selected_indices.insert(index);
        self.focused_index = Some(index);
        self.set_selection_anchor(index);
    }

    pub fn select_range_to(&mut self, index: EntryIndex, additive: bool) {
        if self.visible_indices().is_empty() {
            return;
        }

        let anchor_index = self
            .selection_anchor_index()
            .or(self.focused_index)
            .or_else(|| self.selected_indices.iter().copied().min())
            .unwrap_or(index);

        self.select_range_from(anchor_index, index, additive);
    }

    pub fn select_range_from(
        &mut self,
        anchor_index: EntryIndex,
        index: EntryIndex,
        additive: bool,
    ) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }

        let Some(anchor_position) = self.visible_position(anchor_index) else {
            self.select_single(index);
            return;
        };
        let Some(index_position) = self.visible_position(index) else {
            self.select_single(anchor_index);
            return;
        };

        let (start, end) = if anchor_position <= index_position {
            (anchor_position, index_position)
        } else {
            (index_position, anchor_position)
        };
        let selected_range: Vec<EntryIndex> = visible[start..=end].to_vec();

        if !additive {
            self.selected_indices.clear();
        }

        self.selected_indices.extend(selected_range);
        self.focused_index = Some(index);
        self.selection_anchor_path = self.entry_display_path(anchor_index);
    }

    pub(crate) fn entry_display_path(&self, index: EntryIndex) -> Option<String> {
        self.entries
            .get(entry_offset(index))
            .map(|entry| entry.object_ref.display_path.clone())
    }

    pub(crate) fn selection_anchor_index(&self) -> Option<EntryIndex> {
        self.selection_anchor_path
            .as_deref()
            .and_then(|path| self.entry_index_by_display_path(path))
    }

    fn set_selection_anchor(&mut self, index: EntryIndex) {
        self.selection_anchor_path = self.entry_display_path(index);
    }

    /// Moves focus `delta` visible entries (one row is `items_per_row`), stopping at the ends.
    /// Without focus, the first entry gets it.
    pub fn move_focus_by(&mut self, delta: isize) {
        let visible = self.visible_indices();
        let Some(&first) = visible.first() else {
            return;
        };
        self.focused_index = Some(
            match self
                .focused_index
                .and_then(|index| self.visible_position(index))
            {
                Some(position) => {
                    visible[position.saturating_add_signed(delta).min(visible.len() - 1)]
                }
                None => first,
            },
        );
    }

    pub fn select_focused(&mut self) {
        if let Some(index) = self.focused_index {
            self.select_single(index);
        }
    }

    /// Selected entries in list order.
    fn selected_entries(&self) -> impl Iterator<Item = &iris_core_domain::types::list::ListEntry> {
        let mut indices: Vec<EntryIndex> = self.selected_indices.iter().copied().collect();
        indices.sort_unstable();
        indices
            .into_iter()
            .filter_map(|index| self.entries.get(entry_offset(index)))
    }

    pub fn selected_object_refs(&self) -> Vec<ObjectRef> {
        self.selected_entries()
            .map(|entry| entry.object_ref.clone())
            .collect()
    }

    pub fn selected_names(&self) -> Vec<String> {
        self.selected_entries()
            .map(|entry| entry.name.clone())
            .collect()
    }
}
