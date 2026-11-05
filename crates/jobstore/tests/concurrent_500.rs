use iris_core_domain::types::transfer::{JobId, JobState, TaskId, TaskKind, TaskState};
use iris_jobstore::repository::{PersistedJob, PersistedTask};
use iris_jobstore::writer_task::JobStore;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

fn sample_job(task_count: u64) -> PersistedJob {
    PersistedJob {
        id: JobId(1),
        state: JobState::Running,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        description: "concurrent-500".into(),
        tasks: (0..task_count)
            .map(|index| PersistedTask {
                id: TaskId(index + 1),
                job_id: JobId(1),
                kind: TaskKind::Copy,
                state: TaskState::Running,
                src_backend: Some("s3:bucket".into()),
                src_stable_id: Some(format!("src-{index}")),
                src_display_path: Some(format!("/src/{index}")),
                dst_backend: Some("local".into()),
                dst_stable_id: Some(format!("dst-{index}")),
                dst_display_path: Some(format!("/dst/{index}")),
                bytes_total: 1024,
                bytes_completed: 0,
                error_message: None,
            })
            .collect(),
    }
}

#[test]
fn concurrent_500_progress_updates_keep_reads_responsive() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(JobStore::open(&temp.path().join("jobs.sqlite3")).expect("open store"));
    store.save_job(&sample_job(500)).expect("save job");

    let barrier = Arc::new(Barrier::new(501));
    let mut handles = Vec::new();
    for index in 0..500u64 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            store
                .update_task_progress(&TaskId(index + 1), index + 1)
                .expect("update task progress");
        }));
    }

    barrier.wait();

    let mut max_read_latency = Duration::ZERO;
    for _ in 0..50 {
        let started_at = Instant::now();
        let loaded = store
            .load_all_incomplete_jobs()
            .expect("load incomplete jobs concurrently");
        max_read_latency = max_read_latency.max(started_at.elapsed());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].tasks.len(), 500);
        thread::sleep(Duration::from_millis(2));
    }

    for handle in handles {
        handle.join().expect("writer thread");
    }

    thread::sleep(Duration::from_millis(100));

    let loaded = store
        .load_job(&JobId(1))
        .expect("load persisted job")
        .expect("job should exist");
    let progressed = loaded
        .tasks
        .iter()
        .filter(|task| task.bytes_completed > 0)
        .count();

    assert_eq!(progressed, 500);
    assert!(
        max_read_latency < Duration::from_millis(250),
        "read-side latency regressed to {:?}",
        max_read_latency
    );
}
