use super::*;
use crate::messages::JobMessage;
use iris_core_domain::types::transfer::JobId;

pub fn handle(state: &mut AppState, message: JobMessage) -> Task<Message> {
    match message {
        JobMessage::Pause(job_id) => {
            app_runtime().pause_job(&job_id);
        }
        JobMessage::Resume(job_id) => {
            app_runtime().resume_job(&job_id);
            if restore_pending_operation(state, &job_id) {
                return process_pending_operation(state);
            }
        }
        JobMessage::Cancel(job_id) => {
            app_runtime().cancel_job(&job_id);
            if let Some(pending) = &state.pending_operation {
                if pending.job_id == job_id {
                    state.pending_operation = None;
                    state.active_dialog = None;
                }
            }
        }
        JobMessage::Retry(job_id) => {
            app_runtime().resume_job(&job_id);
            restore_pending_operation(state, &job_id);
            return process_pending_operation(state);
        }
        JobMessage::RuntimeProgress(job_id, progress) => {
            if let Some(job) = state.jobs.iter_mut().find(|j| j.id == job_id) {
                job.progress.bytes_completed = progress.bytes_completed;
                job.progress.bytes_total = progress.bytes_total;
                update_job_speed(job);
            }
        }
        JobMessage::RuntimeState(job_id, job_state) => {
            if let Some(job) = state.jobs.iter_mut().find(|j| j.id == job_id) {
                job.state = job_state.clone();
                if !matches!(job_state, JobState::Running) {
                    reset_job_speed(job);
                }
            }
        }
    }

    Task::none()
}

/// Restores the pending operation of a persisted job unless another operation is running.
fn restore_pending_operation(state: &mut AppState, job_id: &JobId) -> bool {
    if state.pending_operation.is_some() {
        return false;
    }
    let Ok(Some((items, current_index, is_move))) = app_runtime().restore_pending_operation(job_id)
    else {
        return false;
    };
    state.pending_operation = Some(PendingFileOperation {
        items,
        current_index,
        is_move,
        remember_resolution: None,
        job_id: job_id.clone(),
        in_flight: 0,
        rsync_failed: false,
        undo_action: Some(PendingUndoAction::RecordTransfer),
    });
    true
}
