use crate::messages::{EntryMetadataUpdate, Message, PaneMessage};
use crate::runtime::app_runtime;
use crate::state::{list_entry_kind, AppState, SortKey};
use crate::update::thumbnail::visible_index_range;
use iced::Task;
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::ObjectRef;

const METADATA_HYDRATION_OVERSCAN: usize = 64;
const METADATA_HYDRATION_BATCH_LIMIT: usize = 160;

pub(crate) fn schedule_visible_metadata_hydration(
    state: &mut AppState,
    pane_idx: usize,
) -> Task<Message> {
    let target_refs = visible_pending_metadata_targets(state, pane_idx);
    if target_refs.is_empty() {
        return Task::none();
    }

    let mut targets = Vec::with_capacity(target_refs.len());
    {
        let pane = state.pane_mut(pane_idx);
        for object_ref in target_refs {
            if pane
                .metadata_hydration_inflight
                .insert(object_ref.display_path.clone())
            {
                targets.push(object_ref);
            }
        }
    }

    if targets.is_empty() {
        return Task::none();
    }

    Task::perform(hydrate_metadata(targets), move |updates| {
        Message::Pane(pane_idx, PaneMessage::MetadataHydrated(updates))
    })
}

pub(crate) fn apply_metadata_hydration(
    state: &mut AppState,
    pane_idx: usize,
    updates: Vec<EntryMetadataUpdate>,
) -> Task<Message> {
    let mut changed = false;
    let mut kind_changed = false;

    {
        let pane = state.pane_mut(pane_idx);
        for update in updates {
            pane.metadata_hydration_inflight
                .remove(&update.object_ref.display_path);
            let Some(entry) = pane.entry_mut_by_display_path(&update.object_ref.display_path)
            else {
                continue;
            };
            if entry.object_ref.key.stable_id != update.object_ref.key.stable_id {
                continue;
            }

            match update.result {
                Ok(metadata) => {
                    let old_kind = list_entry_kind(entry);
                    let old_is_dir = entry.is_dir;
                    let old_size = entry.size;
                    let was_dir = entry.is_dir;
                    apply_metadata_to_entry(entry, metadata);
                    let new_kind = list_entry_kind(entry);
                    let new_is_dir = entry.is_dir;
                    let new_size = entry.size;
                    app_runtime().cache_list_entry_metadata(
                        &entry.object_ref,
                        if entry.is_dir { None } else { entry.size },
                        entry.mtime,
                        None,
                    );
                    if was_dir != entry.is_dir {
                        kind_changed = true;
                    }
                    changed = true;
                    pane.facet_counts
                        .remove_values(old_kind, old_is_dir, old_size);
                    pane.facet_counts.add_values(new_kind, new_is_dir, new_size);
                }
                Err(_) => {
                    entry.is_inaccessible = true;
                    changed = true;
                }
            }
        }

        if changed {
            if kind_changed || metadata_affects_sort_order(pane.sort_key) {
                pane.sort_entries();
            } else if metadata_affects_visible_projection(pane) {
                pane.rebuild_visible_indices();
            }
        }
    }

    if changed {
        return schedule_visible_metadata_hydration(state, pane_idx);
    }

    Task::none()
}

fn metadata_affects_sort_order(sort_key: SortKey) -> bool {
    matches!(
        sort_key,
        SortKey::Date | SortKey::ModifiedDate | SortKey::CreatedDate | SortKey::Size
    )
}

fn metadata_affects_visible_projection(pane: &crate::state::PaneState) -> bool {
    pane.filter_model.size.is_some() || pane.filter_model.date.is_some()
}

fn visible_pending_metadata_targets(state: &AppState, pane_idx: usize) -> Vec<ObjectRef> {
    let pane = state.pane(pane_idx);
    if metadata_affects_sort_order(pane.sort_key) {
        return pane
            .entries
            .iter()
            .filter(|entry| {
                entry.metadata_pending()
                    && !pane
                        .metadata_hydration_inflight
                        .contains(&entry.object_ref.display_path)
            })
            .take(METADATA_HYDRATION_BATCH_LIMIT)
            .map(|entry| entry.object_ref.clone())
            .collect();
    }
    let visible_indices = pane.visible_indices();
    if visible_indices.is_empty() {
        return Vec::new();
    }

    let (visible_start, visible_end) = visible_index_range(state, pane_idx, visible_indices.len());
    let start = visible_start.saturating_sub(METADATA_HYDRATION_OVERSCAN);
    let end = (visible_end + METADATA_HYDRATION_OVERSCAN).min(visible_indices.len());

    visible_indices[start..end]
        .iter()
        .filter_map(|index| pane.entry(*index))
        .filter(|entry| {
            entry.metadata_pending()
                && !pane
                    .metadata_hydration_inflight
                    .contains(&entry.object_ref.display_path)
        })
        .take(METADATA_HYDRATION_BATCH_LIMIT)
        .map(|entry| entry.object_ref.clone())
        .collect()
}

async fn hydrate_metadata(targets: Vec<ObjectRef>) -> Vec<EntryMetadataUpdate> {
    app_runtime()
        .stat_objects(targets)
        .await
        .into_iter()
        .map(|(object_ref, result)| EntryMetadataUpdate { object_ref, result })
        .collect()
}

fn apply_metadata_to_entry(entry: &mut ListEntry, metadata: MetadataResult) {
    entry.metadata_level = metadata.level.max(MetadataLevel::L1);
    entry.is_dir = metadata.is_dir;
    entry.size = if metadata.is_dir { None } else { metadata.size };
    entry.mtime = metadata.mtime;
    entry.ctime = metadata.ctime;
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::object::{BackendId, ObjectKey, StableId};

    fn entry(path: &str, index: usize, level: MetadataLevel) -> ListEntry {
        let name = format!("file-{index}.txt");
        ListEntry {
            object_ref: ObjectRef {
                key: ObjectKey {
                    backend_id: BackendId("local".to_string()),
                    stable_id: StableId(format!("stable-{index}")),
                },
                parent_stable_id: None,
                display_path: path.to_string(),
            },
            sort_name_key: ListEntry::sort_name_key_for(&name),
            sort_extension_key: ListEntry::sort_extension_key_for(&name),
            name,
            is_dir: false,
            metadata_level: level,
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
    fn visible_pending_metadata_targets_skip_hydrated_and_inflight_entries() {
        let mut state = AppState::default();
        state.window_height = 200.0;
        state.pane_mut(0).entries = vec![
            entry("/tmp/a.txt", 0, MetadataLevel::L0),
            entry("/tmp/b.txt", 1, MetadataLevel::L1),
            entry("/tmp/c.txt", 2, MetadataLevel::L0),
        ];
        state.pane_mut(0).rebuild_visible_indices();
        state
            .pane_mut(0)
            .metadata_hydration_inflight
            .insert("/tmp/c.txt".to_string());

        let targets = visible_pending_metadata_targets(&state, 0);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].display_path, "/tmp/a.txt");
    }

    #[test]
    fn apply_metadata_hydration_uses_path_index_and_ignores_stale_ids() {
        let mut state = AppState::default();
        state.pane_mut(0).replace_entries(vec![
            entry("/tmp/a.txt", 0, MetadataLevel::L1),
            entry("/tmp/b.txt", 1, MetadataLevel::L0),
        ]);
        state
            .pane_mut(0)
            .metadata_hydration_inflight
            .insert("/tmp/a.txt".to_string());
        state
            .pane_mut(0)
            .metadata_hydration_inflight
            .insert("/tmp/b.txt".to_string());

        let stale_a = ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("local".to_string()),
                stable_id: StableId("stale-a".to_string()),
            },
            parent_stable_id: None,
            display_path: "/tmp/a.txt".to_string(),
        };
        let current_b = state.pane(0).entries[1].object_ref.clone();
        let _ = apply_metadata_hydration(
            &mut state,
            0,
            vec![
                EntryMetadataUpdate {
                    object_ref: stale_a,
                    result: Ok(MetadataResult {
                        level: MetadataLevel::L1,
                        is_dir: false,
                        size: Some(10),
                        mtime: None,
                        ctime: None,
                        content_type: None,
                        etag: None,
                    }),
                },
                EntryMetadataUpdate {
                    object_ref: current_b,
                    result: Ok(MetadataResult {
                        level: MetadataLevel::L1,
                        is_dir: false,
                        size: Some(20),
                        mtime: None,
                        ctime: None,
                        content_type: None,
                        etag: None,
                    }),
                },
            ],
        );

        assert_eq!(state.pane(0).entries[0].metadata_level, MetadataLevel::L1);
        assert_eq!(state.pane(0).entries[0].size, None);
        assert_eq!(state.pane(0).entries[1].metadata_level, MetadataLevel::L1);
        assert_eq!(state.pane(0).entries[1].size, Some(20));
        assert!(state.pane(0).metadata_hydration_inflight.is_empty());
    }

    #[test]
    fn apply_metadata_hydration_rebuilds_projection_when_size_filter_changes() {
        let mut state = AppState::default();
        state
            .pane_mut(0)
            .replace_entries(vec![entry("/tmp/a.txt", 0, MetadataLevel::L0)]);
        state.pane_mut(0).set_filter_query("size:>10".to_string());
        assert!(state.pane(0).visible_indices().is_empty());

        let object_ref = state.pane(0).entries[0].object_ref.clone();
        let _ = apply_metadata_hydration(
            &mut state,
            0,
            vec![EntryMetadataUpdate {
                object_ref,
                result: Ok(MetadataResult {
                    level: MetadataLevel::L1,
                    is_dir: false,
                    size: Some(20),
                    mtime: None,
                    ctime: None,
                    content_type: None,
                    etag: None,
                }),
            }],
        );

        assert_eq!(state.pane(0).visible_indices(), &[0]);
    }

    #[test]
    fn apply_metadata_hydration_updates_entry_kind_and_grid_projection() {
        let mut state = AppState::default();
        state
            .pane_mut(0)
            .replace_entries(vec![entry("/tmp/link-to-dir", 0, MetadataLevel::L0)]);

        let object_ref = state.pane(0).entries[0].object_ref.clone();
        let _ = apply_metadata_hydration(
            &mut state,
            0,
            vec![EntryMetadataUpdate {
                object_ref,
                result: Ok(MetadataResult {
                    level: MetadataLevel::L1,
                    is_dir: true,
                    size: Some(20),
                    mtime: None,
                    ctime: None,
                    content_type: None,
                    etag: None,
                }),
            }],
        );

        let pane = state.pane(0);
        assert!(pane.entries[0].is_dir);
        assert_eq!(pane.entries[0].size, None);
        assert_eq!(pane.visible_folder_indices(), &[0]);
        assert!(pane.visible_file_indices().is_empty());
    }
}
