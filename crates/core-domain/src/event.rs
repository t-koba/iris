use crate::types::progress::ProgressInfo;
use crate::types::transfer::{JobId, JobState};
use tokio::sync::broadcast;

/// Job events published by JobManager and consumed by the UI shell.
#[derive(Clone, Debug)]
pub enum DomainEvent {
    JobProgress {
        job_id: JobId,
        progress: ProgressInfo,
    },

    JobStateChanged {
        job_id: JobId,
        new_state: JobState,
    },
}

pub struct EventBus {
    sender: broadcast::Sender<DomainEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Events are dropped when nobody is subscribed.
    pub fn publish(&self, event: DomainEvent) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.sender.subscribe()
    }
}
