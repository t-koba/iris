use crate::input::dnd::{DndOperation, DndState, DndTarget};
use crate::messages::{JobMessage, Message, PendingItemResult};
use crate::runtime::app_runtime;
use crate::state::{
    AppState, JobDisplayInfo, NotificationAction, NotificationSeverity, PendingFileItem,
    PendingFileOperation,
};
use crate::undo::{UndoItem, UndoOpKind};
use crate::update::undo_ops::record_undo_op_task;
use iced::Task;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::options::ConflictResolution;
use iris_core_domain::types::transfer::{JobId, JobState};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

mod dnd_ops;
mod pending_ops;

pub use dnd_ops::{current_dnd_operation, perform_dnd_drop, perform_external_drop};
pub(crate) use pending_ops::max_concurrent_transfers_for_items;
pub use pending_ops::{
    apply_conflict_resolution, cancel_job_state, dismiss_job_decisions, fail_pending_job,
    process_pending_operation, resolve_failed_items, start_pending_file_operation,
    start_pending_file_operation_with_undo_action,
};

const JOB_SPEED_SMOOTHING_WINDOW: Duration = Duration::from_secs(3);
const JOB_SPEED_MIN_SAMPLE_ELAPSED: Duration = Duration::from_millis(400);

pub fn next_job_id(state: &AppState) -> JobId {
    let next = state.jobs.iter().map(|j| j.id.0).max().unwrap_or(0) + 1;
    JobId(next)
}

pub fn file_name_from_path(path: &str) -> String {
    Path::new(path.trim_end_matches(['/', '\\']))
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn update_job_speed(job: &mut JobDisplayInfo) {
    let now = std::time::Instant::now();
    let current = job.progress.bytes_completed;

    if job
        .speed_samples
        .back()
        .map(|(_, bytes)| current < *bytes)
        .unwrap_or(false)
    {
        job.speed_samples.clear();
    }

    if job
        .speed_samples
        .back()
        .map(|(_, bytes)| current != *bytes)
        .unwrap_or(true)
    {
        job.speed_samples.push_back((now, current));
    }

    while job.speed_samples.len() > 1 {
        let remove = job
            .speed_samples
            .front()
            .map(|(sample_at, _)| now.duration_since(*sample_at) > JOB_SPEED_SMOOTHING_WINDOW)
            .unwrap_or(false);
        if !remove {
            break;
        }
        job.speed_samples.pop_front();
    }

    let Some((oldest_at, oldest_bytes)) = job.speed_samples.front().copied() else {
        job.speed_bps = None;
        return;
    };

    let elapsed = now.duration_since(oldest_at);
    let delta = current.saturating_sub(oldest_bytes);
    if delta > 0 && elapsed >= JOB_SPEED_MIN_SAMPLE_ELAPSED {
        job.speed_bps = Some(delta as f64 / elapsed.as_secs_f64());
    }
}

pub fn reset_job_speed(job: &mut JobDisplayInfo) {
    job.speed_samples.clear();
    job.speed_bps = None;
}

pub fn open_object_task(object_ref: ObjectRef) -> Task<Message> {
    Task::perform(
        async move { app_runtime().open_object(&object_ref).await },
        Message::OpenObjectCompleted,
    )
}
