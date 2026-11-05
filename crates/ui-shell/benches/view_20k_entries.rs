use criterion::{criterion_group, criterion_main, Criterion};
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::MetadataLevel;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_ui_shell::app;
use iris_ui_shell::state::{AppState, PaneState, PreviewPosition, PropertiesPosition};
use std::time::{Duration, SystemTime};

fn object_ref(backend_id: &str, display_path: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId(backend_id.to_string()),
            stable_id: StableId(display_path.to_string()),
        },
        parent_stable_id: None,
        display_path: display_path.to_string(),
    }
}

fn list_entry(backend_id: &str, display_path: &str, is_dir: bool, size: Option<u64>) -> ListEntry {
    let name = std::path::PathBuf::from(display_path)
        .file_name()
        .and_then(|name| name.to_str())
        .expect("file name")
        .to_string();
    ListEntry {
        metadata_level: MetadataLevel::L1,
        size,
        mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        ctime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_900_000)),
        ..ListEntry::new(object_ref(backend_id, display_path), name, is_dir)
    }
}

fn pane_with_entries(current_path: &str, entries: Vec<ListEntry>) -> PaneState {
    let mut pane = PaneState::default();
    pane.current_path = current_path.to_string();
    pane.entries = entries;
    pane.rebuild_visible_indices();
    pane.navigation_history.push(current_path.to_string());
    pane.history_index = pane.navigation_history.len();
    pane.rebuild_breadcrumbs();
    pane
}

fn state_with_20k_entries() -> AppState {
    let mut entries = Vec::with_capacity(20_000);
    for index in 0..20_000usize {
        let path = format!("/Users/test/Pictures/asset-{index:05}.jpg");
        entries.push(list_entry(
            "bench-local",
            &path,
            false,
            Some(256 * 1024 + index as u64),
        ));
    }

    let mut state = AppState::default();
    state.preview_panel_visible = true;
    state.preview_position = PreviewPosition::Right;
    state.properties_position = PropertiesPosition::BelowPreview;
    *state.pane_mut(0) = pane_with_entries("/Users/test/Pictures", entries);
    state.pane_mut(0).select_single(0);
    state
}

fn bench_view_20k_entries(c: &mut Criterion) {
    let state = state_with_20k_entries();
    c.bench_function("view_20k_entries/layout", |b| {
        b.iter(|| {
            let view = app::view_for_test(std::hint::black_box(&state));
            std::hint::black_box(view);
        });
    });
}

criterion_group!(benches, bench_view_20k_entries);
criterion_main!(benches);
