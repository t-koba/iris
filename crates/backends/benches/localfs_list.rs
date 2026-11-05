#[path = "common/mod.rs"]
mod support;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use iris_backends::localfs::adapter::LocalFsAdapter;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::list::ListCursor;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use std::time::Duration;
use support::synthetic_tree::SyntheticTreeSpec;
use tempfile::TempDir;
use tokio::runtime::Runtime;

fn root_ref(backend_id: &BackendId) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: backend_id.clone(),
            stable_id: StableId("root".to_string()),
        },
        parent_stable_id: None,
        display_path: String::new(),
    }
}

async fn list_all_pages(adapter: &LocalFsAdapter, root: &ObjectRef) -> usize {
    let mut cursor: Option<ListCursor> = None;
    let mut total = 0usize;
    loop {
        let page = adapter
            .list_names(root, cursor)
            .await
            .expect("localfs list page");
        total += page.entries.len();
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    total
}

fn configured_sizes() -> Vec<usize> {
    std::env::var("IRIS_PERF_TREE_SIZES")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| part.parse::<usize>().expect("numeric tree size"))
                .collect::<Vec<_>>()
        })
        .filter(|sizes| !sizes.is_empty())
        .unwrap_or_else(|| vec![1_000, 10_000])
}

fn bench_localfs_list(c: &mut Criterion) {
    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("localfs_list");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    for entries in configured_sizes() {
        let temp_dir = TempDir::new().expect("synthetic tree temp dir");
        let tree = SyntheticTreeSpec::flat(entries)
            .materialize(temp_dir.path())
            .expect("materialize synthetic tree");
        let backend_id = BackendId(format!("bench-localfs-{entries}"));
        let adapter = LocalFsAdapter::new(tree.root().to_path_buf(), backend_id.clone());
        let root = root_ref(&backend_id);
        let listed = runtime.block_on(list_all_pages(&adapter, &root));
        assert!(
            listed >= tree.entries(),
            "list should include all flat synthetic entries plus deep-root"
        );

        group.throughput(Throughput::Elements(tree.entries() as u64));
        group.bench_with_input(
            BenchmarkId::new("paged_full_directory", entries),
            &entries,
            |b, _| {
                b.iter(|| {
                    let listed = runtime.block_on(list_all_pages(&adapter, &root));
                    std::hint::black_box(listed);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_localfs_list);
criterion_main!(benches);
