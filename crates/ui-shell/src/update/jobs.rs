use super::*;
use crate::messages::JobMessage;
use iris_core_domain::types::transfer::JobId;

pub fn handle(state: &mut AppState, message: JobMessage) -> Task<Message> {
    match message {
        JobMessage::Persisted(job_id) => {
            if let Some(pending) = &mut state.pending_operation {
                if pending.job_id == job_id {
                    pending.persisted = true;
                    return process_pending_operation(state);
                }
            }
            // Canceled before it was stored: storing it recorded it as queued, so record the
            // cancellation again or it would be offered for resuming after a restart.
            if state
                .jobs
                .iter()
                .any(|job| job.id == job_id && job.state == JobState::Canceled)
            {
                app_runtime().cancel_job(&job_id);
            }
        }
        JobMessage::Pause(job_id) => {
            app_runtime().pause_job(&job_id);
            // Stop starting new files right away, not only once the runtime reports the state.
            set_job_state_if(state, &job_id, JobState::Running, JobState::Paused);
        }
        JobMessage::Resume(job_id) => {
            app_runtime().resume_job(&job_id);
            set_job_state_if(state, &job_id, JobState::Paused, JobState::Running);
            let resumes_this_session = state
                .pending_operation
                .as_ref()
                .is_some_and(|pending| pending.job_id == job_id);
            if resumes_this_session || restore_pending_operation(state, &job_id) {
                return process_pending_operation(state);
            }
        }
        JobMessage::Cancel(job_id) => cancel_job_state(state, &job_id),
        JobMessage::Retry(job_id) => {
            let failed_in_this_session = state
                .pending_operation
                .as_ref()
                .is_some_and(|pending| pending.job_id == job_id);
            if failed_in_this_session {
                return resolve_failed_items(state, &job_id, true);
            }
            // A job interrupted in an earlier session: resume it from the job store.
            dismiss_job_decisions(state, &job_id);
            app_runtime().resume_job(&job_id);
            if restore_pending_operation(state, &job_id) {
                if let Some(job) = state.jobs.iter_mut().find(|job| job.id == job_id) {
                    job.state = JobState::Running;
                }
                return process_pending_operation(state);
            }
        }
        JobMessage::RuntimeProgress(job_id, progress) => {
            if let Some(job) = state.jobs.iter_mut().find(|j| j.id == job_id) {
                job.progress.bytes_completed = progress.bytes_completed;
                job.progress.bytes_total = progress.bytes_total;
                update_job_speed(job);
            }
        }
        JobMessage::RuntimeState(job_id, job_state) => {
            // Files still finishing report Running, but the job waits for the user's decision.
            let awaits_decision = state
                .pending_operation
                .as_ref()
                .is_some_and(|pending| pending.job_id == job_id && !pending.failed.is_empty());
            if awaits_decision && job_state == JobState::Running {
                return Task::none();
            }
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

fn set_job_state_if(state: &mut AppState, job_id: &JobId, from: JobState, to: JobState) {
    if let Some(job) = state
        .jobs
        .iter_mut()
        .find(|job| job.id == *job_id && job.state == from)
    {
        job.state = to;
    }
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
    let max_in_flight = super::file_ops::max_concurrent_transfers_for_items(&items);
    let mut pending = PendingFileOperation::new(
        job_id.clone(),
        items,
        is_move,
        Some(PendingUndoAction::RecordTransfer),
        max_in_flight,
    );
    pending.current_index = current_index;
    // Restored from the job store, so it is already persisted.
    pending.persisted = true;
    state.pending_operation = Some(pending);
    true
}
