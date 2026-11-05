use crate::method_selector::recommended_workers_for_backend;
use iris_core_domain::event::{DomainEvent, EventBus};
use iris_core_domain::types::progress::ProgressInfo;
use iris_core_domain::types::transfer::{JobId, JobState};
use iris_jobstore::repository::PersistedTask;
use iris_jobstore::writer_task::JobStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::info;

/// Concurrency limits, optionally per backend.
pub struct ConcurrencyLimits {
    pub default_max: usize,
    pub per_backend: HashMap<String, usize>,
}

impl Default for ConcurrencyLimits {
    fn default() -> Self {
        Self {
            default_max: 4,
            per_backend: HashMap::new(),
        }
    }
}

struct ManagedJob {
    state: JobState,
    backend_types: Vec<String>,
    progress: ProgressInfo,
}

#[derive(Clone, Debug)]
pub struct RestoredManagedJob {
    pub id: JobId,
    pub description: String,
    pub state: JobState,
    pub total_files: u64,
}

pub struct JobManager {
    jobs: Mutex<HashMap<u64, ManagedJob>>,
    job_store: Arc<JobStore>,
    event_bus: Arc<EventBus>,
    limits: ConcurrencyLimits,
}

impl JobManager {
    pub fn new(
        job_store: Arc<JobStore>,
        event_bus: Arc<EventBus>,
        limits: ConcurrencyLimits,
    ) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            job_store,
            event_bus,
            limits,
        }
    }

    pub fn track_job(
        &self,
        id: JobId,
        state: JobState,
        total_files: u64,
        total_bytes: Option<u64>,
        backend_types: Vec<String>,
    ) {
        let job = ManagedJob {
            state,
            backend_types,
            progress: job_progress(total_files, total_bytes),
        };

        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.0, job);
    }

    pub fn update_state(&self, job_id: &JobId, new_state: JobState) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = jobs.get_mut(&job_id.0) {
            job.state = new_state.clone();
            self.event_bus.publish(DomainEvent::JobStateChanged {
                job_id: job_id.clone(),
                new_state,
            });
        }
    }

    pub fn pause_job(&self, job_id: &JobId) {
        self.update_state(job_id, JobState::Paused);
        info!("paused job {}", job_id.0);
    }

    pub fn resume_job(&self, job_id: &JobId) {
        self.update_state(job_id, JobState::Running);
        info!("resumed job {}", job_id.0);
    }

    pub fn cancel_job(&self, job_id: &JobId) {
        self.update_state(job_id, JobState::Canceled);
        info!("canceled job {}", job_id.0);
    }

    pub fn update_progress(&self, job_id: &JobId, bytes_completed: u64) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = jobs.get_mut(&job_id.0) {
            job.progress.bytes_completed = bytes_completed;

            self.event_bus.publish(DomainEvent::JobProgress {
                job_id: job_id.clone(),
                progress: job.progress.clone(),
            });
        }
    }

    pub fn get_state(&self, job_id: &JobId) -> Option<JobState> {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.get(&job_id.0).map(|j| j.state.clone())
    }

    /// Restores incomplete jobs after an app restart.
    pub fn restore_jobs<F>(
        &self,
        mut is_task_reachable: F,
    ) -> Result<Vec<RestoredManagedJob>, iris_jobstore::repository::JobStoreError>
    where
        F: FnMut(&PersistedTask) -> bool,
    {
        let persisted = self.job_store.load_all_incomplete_jobs()?;
        let mut restored = Vec::new();

        for pj in persisted {
            let id = pj.id.clone();
            let total_files = pj.tasks.len() as u64;
            let backend_types = collect_backend_types(&pj.tasks);
            let restored_state = if pj.tasks.iter().all(&mut is_task_reachable) {
                match pj.state {
                    JobState::Paused => JobState::Paused,
                    _ => JobState::Queued,
                }
            } else {
                JobState::UnknownRemoteState
            };

            let job = ManagedJob {
                state: restored_state.clone(),
                backend_types,
                progress: job_progress(total_files, None),
            };

            self.jobs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.0, job);

            info!("restored job {}: {}", id.0, pj.description);
            restored.push(RestoredManagedJob {
                id,
                description: pj.description,
                state: restored_state,
                total_files,
            });
        }

        Ok(restored)
    }

    pub fn can_start_job(&self, backend_type: &str) -> bool {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let running_count = jobs
            .values()
            .filter(|j| {
                j.state == JobState::Running
                    && j.backend_types.iter().any(|kind| kind == backend_type)
            })
            .count();

        let limit = self
            .limits
            .per_backend
            .get(backend_type)
            .copied()
            .unwrap_or(self.limits.default_max);
        let limit = recommended_workers_for_backend(backend_type, limit);

        running_count < limit
    }
}

fn job_progress(total_files: u64, total_bytes: Option<u64>) -> ProgressInfo {
    ProgressInfo {
        bytes_completed: 0,
        bytes_total: total_bytes,
        files_completed: 0,
        files_total: Some(total_files),
    }
}

fn collect_backend_types(tasks: &[PersistedTask]) -> Vec<String> {
    let mut backend_types = Vec::new();
    for task in tasks {
        for backend in [&task.src_backend, &task.dst_backend] {
            let Some(backend) = backend else {
                continue;
            };
            let backend_type = backend.split(':').next().unwrap_or(backend).to_string();
            if !backend_types.contains(&backend_type) {
                backend_types.push(backend_type);
            }
        }
    }
    backend_types
}
