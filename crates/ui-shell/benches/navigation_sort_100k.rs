use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::MetadataLevel;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_ui_shell::state::{PaneState, SortKey, SortOrder};
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

fn list_entry(backend_id: &str, index: usize) -> ListEntry {
    let is_dir = index.is_multiple_of(31);
    let name = match index % 17 {
        0 => format!("写真-{index:07}-東京.jpg"),
        1 => format!("report-{index:07}.PDF"),
        2 => format!("Archive-{index:07}.tar.gz"),
        _ if is_dir => format!("Folder-{index:07}"),
        _ => format!("asset-{index:07}.dat"),
    };
    let display_path = format!("/bench/{name}");
    ListEntry {
        metadata_level: MetadataLevel::L1,
        size: (!is_dir).then_some(512 + index as u64 * 37),
        mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + index as u64)),
        ctime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_690_000_000 + index as u64)),
        ..ListEntry::new(object_ref(backend_id, &display_path), name, is_dir)
    }
}

fn entries(count: usize) -> Vec<ListEntry> {
    (0..count)
        .rev()
        .map(|index| list_entry("bench-navigation", index))
        .collect()
}

fn pane_with_entries(entries: Vec<ListEntry>) -> PaneState {
    let mut pane = PaneState::default();
    pane.current_path = "/bench".to_string();
    pane.entries = entries;
    pane.sort_key = SortKey::Name;
    pane.sort_order = SortOrder::Ascending;
    pane.rebuild_visible_indices();
    for index in (0..pane.entries.len()).step_by(97) {
        pane.selected_indices.insert(index as u32);
    }
    pane.focused_index = Some((pane.entries.len() / 2) as u32);
    pane
}

fn bench_navigation_sort(c: &mut Criterion) {
    let entries = entries(100_000);
    let mut group = c.benchmark_group("navigation_100k");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("sort_entries/name_projection", |b| {
        b.iter_batched(
            || pane_with_entries(entries.clone()),
            |mut pane| {
                pane.sort_key = SortKey::Name;
                pane.sort_entries();
                std::hint::black_box((pane.focused_index, pane.selected_indices.len()));
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("sort_entries/file_type_projection", |b| {
        b.iter_batched(
            || pane_with_entries(entries.clone()),
            |mut pane| {
                pane.sort_key = SortKey::FileType;
                pane.sort_entries();
                std::hint::black_box((pane.focused_index, pane.selected_indices.len()));
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("filter_query/recompute_visible_indices", |b| {
        b.iter_batched(
            || pane_with_entries(entries.clone()),
            |mut pane| {
                pane.set_filter_query("東京".to_string());
                std::hint::black_box((pane.focused_index, pane.selected_indices.len()));
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_navigation_sort);
criterion_main!(benches);
