use iris_core_domain::types::object::ObjectKey;
use iris_core_domain::types::thumbnail::ThumbnailHint;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

const PRIORITY_QUEUE_CAPACITY: usize = 1_024;

#[derive(Debug)]
pub struct ThumbnailRequest {
    pub object_key: ObjectKey,
    pub hint: ThumbnailHint,
    pub content_type: Option<String>,
    pub priority: RequestPriority,
}

/// Visible and nearby items are fetched first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RequestPriority {
    Visible,
    /// Near the viewport (prefetch).
    Nearby,
    Background,
}

struct PriorityQueue {
    sender: mpsc::Sender<ThumbnailRequest>,
    receiver: mpsc::Receiver<ThumbnailRequest>,
}

impl Default for PriorityQueue {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel(PRIORITY_QUEUE_CAPACITY);
        Self { sender, receiver }
    }
}

impl PriorityQueue {
    fn push(&self, request: ThumbnailRequest) -> Result<(), ThumbnailRequest> {
        self.sender
            .try_send(request)
            .map_err(|error| error.into_inner())
    }

    fn pop(&mut self) -> Option<ThumbnailRequest> {
        self.receiver.try_recv().ok()
    }

    fn retain(&mut self, mut keep: impl FnMut(&ThumbnailRequest) -> bool) {
        let mut retained = Vec::new();
        while let Ok(request) = self.receiver.try_recv() {
            if keep(&request) {
                retained.push(request);
            }
        }
        for request in retained {
            self.sender
                .try_send(request)
                .expect("retained requests fit in their original bounded queue");
        }
    }

    fn take(&mut self, object_key: &ObjectKey) -> Option<ThumbnailRequest> {
        let mut retained = Vec::new();
        let mut removed = None;
        while let Ok(request) = self.receiver.try_recv() {
            if removed.is_none() && &request.object_key == object_key {
                removed = Some(request);
            } else {
                retained.push(request);
            }
        }
        for request in retained {
            self.sender
                .try_send(request)
                .expect("retained requests fit in their original bounded queue");
        }
        removed
    }

    fn clear(&mut self) {
        while self.receiver.try_recv().is_ok() {}
    }
}

#[derive(Default)]
struct ThumbnailQueues {
    visible: PriorityQueue,
    nearby: PriorityQueue,
    background: PriorityQueue,
    queued_priorities: HashMap<ObjectKey, RequestPriority>,
}

impl ThumbnailQueues {
    fn push(&mut self, request: ThumbnailRequest) -> bool {
        let previous = if let Some(existing_priority) =
            self.queued_priorities.get(&request.object_key).copied()
        {
            if existing_priority <= request.priority {
                return true;
            }
            self.queue_mut(existing_priority).take(&request.object_key)
        } else {
            None
        };
        let object_key = request.object_key.clone();
        let priority = request.priority;
        let result = self.queue_mut(priority).push(request);
        if result.is_ok() {
            self.queued_priorities.insert(object_key, priority);
            true
        } else {
            if let Some(previous) = previous {
                let previous_priority = previous.priority;
                self.queue_mut(previous_priority)
                    .push(previous)
                    .expect("promotion frees one slot in the original queue");
                self.queued_priorities.insert(object_key, previous_priority);
            } else {
                self.queued_priorities.remove(&object_key);
            }
            false
        }
    }

    fn pop(&mut self) -> Option<ThumbnailRequest> {
        loop {
            let request = self
                .visible
                .pop()
                .or_else(|| self.nearby.pop())
                .or_else(|| self.background.pop())?;
            let Some(priority) = self.queued_priorities.get(&request.object_key).copied() else {
                continue;
            };
            if priority != request.priority {
                continue;
            }
            self.queued_priorities.remove(&request.object_key);
            return Some(request);
        }
    }

    fn clear(&mut self) {
        self.visible.clear();
        self.nearby.clear();
        self.background.clear();
        self.queued_priorities.clear();
    }

    fn cancel(&mut self, object_key: &ObjectKey) {
        self.queued_priorities.remove(object_key);
        self.remove_from_queues(object_key);
    }

    fn cancel_batch(&mut self, object_keys: &HashSet<&ObjectKey>) {
        self.queued_priorities
            .retain(|object_key, _| !object_keys.contains(object_key));
        self.visible
            .retain(|req| !object_keys.contains(&req.object_key));
        self.nearby
            .retain(|req| !object_keys.contains(&req.object_key));
        self.background
            .retain(|req| !object_keys.contains(&req.object_key));
    }

    fn has_visible_pending(&self) -> bool {
        self.queued_priorities
            .values()
            .any(|priority| *priority == RequestPriority::Visible)
    }

    fn pending_count(&self) -> usize {
        self.queued_priorities.len()
    }

    fn remove_from_queues(&mut self, object_key: &ObjectKey) {
        self.visible.retain(|req| &req.object_key != object_key);
        self.nearby.retain(|req| &req.object_key != object_key);
        self.background.retain(|req| &req.object_key != object_key);
    }

    fn queue_mut(&mut self, priority: RequestPriority) -> &mut PriorityQueue {
        match priority {
            RequestPriority::Visible => &mut self.visible,
            RequestPriority::Nearby => &mut self.nearby,
            RequestPriority::Background => &mut self.background,
        }
    }
}

/// Prioritizes on-screen thumbnails across bounded per-priority queues.
pub struct ThumbnailScheduler {
    /// Bounded per-priority Tokio channels, owned solely by the UI state.
    queues: ThumbnailQueues,
    max_workers: usize,
    backend_type: Option<String>,
}

impl ThumbnailScheduler {
    pub fn new(max_workers: usize) -> Self {
        Self {
            queues: ThumbnailQueues::default(),
            max_workers,
            backend_type: None,
        }
    }

    pub fn new_for_backend(backend_type: &str) -> Self {
        Self {
            queues: ThumbnailQueues::default(),
            max_workers: base_workers_for_backend(backend_type),
            backend_type: Some(backend_type.to_string()),
        }
    }

    pub fn enqueue(&mut self, request: ThumbnailRequest) -> bool {
        self.queues.push(request)
    }

    /// Visible > Nearby > Background.
    pub fn dequeue(&mut self) -> Option<ThumbnailRequest> {
        self.queues.pop()
    }

    /// Clears every queue, e.g. after changing directory.
    pub fn clear(&mut self) {
        self.queues.clear();
    }

    /// Used to drop requests for items that scrolled off screen.
    pub fn cancel(&mut self, object_key: &ObjectKey) {
        self.queues.cancel(object_key);
    }

    pub fn cancel_batch(&mut self, object_keys: &[ObjectKey]) {
        if object_keys.is_empty() {
            return;
        }
        let object_keys = object_keys.iter().collect::<HashSet<_>>();

        self.queues.cancel_batch(&object_keys);
    }

    pub fn max_workers(&self) -> usize {
        self.backend_type
            .as_deref()
            .map(recommended_workers_for_backend)
            .unwrap_or(self.max_workers)
    }

    pub fn has_visible_pending(&self) -> bool {
        self.queues.has_visible_pending()
    }

    pub fn pending_count(&self) -> usize {
        self.queues.pending_count()
    }
}

/// Thumbnail concurrency per backend type.
pub fn recommended_workers_for_backend(backend_type: &str) -> usize {
    thumbnail_governor().recommended_workers(backend_type, base_workers_for_backend(backend_type))
}

pub fn observe_thumbnail_latency(backend_type: &str, latency: Duration) -> usize {
    thumbnail_governor().observe(backend_type, latency)
}

fn base_workers_for_backend(backend_type: &str) -> usize {
    match backend_type {
        "localfs" | "s3" | "pcloud" => 8,
        // SMB/NFS latency is hidden by parallelism; header reads are cheap for the server.
        "smb" | "nfs" => 6,
        _ => 4,
    }
}

#[derive(Clone, Copy, Debug)]
struct ThumbnailGovernorState {
    ewma_latency_ms: f64,
}

#[derive(Debug)]
struct ThumbnailConcurrencyGovernor {
    alpha: f64,
    target_latency_ms: f64,
    min: usize,
    max: usize,
    states: Mutex<HashMap<String, ThumbnailGovernorState>>,
}

impl Default for ThumbnailConcurrencyGovernor {
    fn default() -> Self {
        Self {
            alpha: 0.3,
            target_latency_ms: 200.0,
            min: 1,
            max: 16,
            states: Mutex::new(HashMap::new()),
        }
    }
}

impl ThumbnailConcurrencyGovernor {
    fn observe(&self, backend_type: &str, latency: Duration) -> usize {
        let sample_latency_ms = latency.as_secs_f64().max(0.001) * 1000.0;
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let state = states
            .entry(backend_type.to_string())
            .or_insert(ThumbnailGovernorState {
                ewma_latency_ms: sample_latency_ms,
            });
        state.ewma_latency_ms =
            self.alpha * sample_latency_ms + (1.0 - self.alpha) * state.ewma_latency_ms;
        self.scaled_workers(base_workers_for_backend(backend_type), *state)
    }

    fn recommended_workers(&self, backend_type: &str, baseline: usize) -> usize {
        let states = self
            .states
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(state) = states.get(backend_type).copied() else {
            return baseline.clamp(self.min, self.max);
        };
        self.scaled_workers(baseline, state)
    }

    fn scaled_workers(&self, baseline: usize, state: ThumbnailGovernorState) -> usize {
        let latency_factor = (self.target_latency_ms / state.ewma_latency_ms).clamp(0.5, 2.0);
        ((baseline as f64 * latency_factor).round() as usize).clamp(self.min, self.max)
    }
}

fn thumbnail_governor() -> &'static ThumbnailConcurrencyGovernor {
    static GOVERNOR: OnceLock<ThumbnailConcurrencyGovernor> = OnceLock::new();
    GOVERNOR.get_or_init(ThumbnailConcurrencyGovernor::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::object::{BackendId, StableId};
    use iris_core_domain::types::thumbnail::ImageFormat;

    fn key(value: &str) -> ObjectKey {
        ObjectKey {
            backend_id: BackendId("local".to_string()),
            stable_id: StableId(value.to_string()),
        }
    }

    fn request(value: &str, priority: RequestPriority) -> ThumbnailRequest {
        ThumbnailRequest {
            object_key: key(value),
            hint: ThumbnailHint {
                max_dimension: 128,
                preferred_formats: vec![ImageFormat::Jpeg],
            },
            content_type: None,
            priority,
        }
    }

    #[test]
    fn cancel_batch_removes_targets_across_priority_queues() {
        let mut scheduler = ThumbnailScheduler::new(4);
        scheduler.enqueue(request("visible-keep", RequestPriority::Visible));
        scheduler.enqueue(request("visible-drop", RequestPriority::Visible));
        scheduler.enqueue(request("nearby-drop", RequestPriority::Nearby));
        scheduler.enqueue(request("background-keep", RequestPriority::Background));

        scheduler.cancel_batch(&[key("visible-drop"), key("nearby-drop")]);

        assert_eq!(scheduler.pending_count(), 2);
        assert_eq!(
            scheduler.dequeue().map(|request| request.object_key),
            Some(key("visible-keep"))
        );
        assert_eq!(
            scheduler.dequeue().map(|request| request.object_key),
            Some(key("background-keep"))
        );
        assert!(scheduler.dequeue().is_none());
    }

    #[test]
    fn enqueue_coalesces_duplicate_object_keys() {
        let mut scheduler = ThumbnailScheduler::new(4);
        scheduler.enqueue(request("same", RequestPriority::Nearby));
        scheduler.enqueue(request("same", RequestPriority::Nearby));

        assert_eq!(scheduler.pending_count(), 1);
        assert_eq!(
            scheduler.dequeue().map(|request| request.object_key),
            Some(key("same"))
        );
        assert!(scheduler.dequeue().is_none());
    }

    #[test]
    fn enqueue_promotes_existing_request_to_higher_priority() {
        let mut scheduler = ThumbnailScheduler::new(4);
        scheduler.enqueue(request("first-visible", RequestPriority::Visible));
        scheduler.enqueue(request("promoted", RequestPriority::Background));
        scheduler.enqueue(request("promoted", RequestPriority::Visible));
        scheduler.enqueue(request("nearby", RequestPriority::Nearby));

        assert_eq!(scheduler.pending_count(), 3);
        assert_eq!(
            scheduler.dequeue().map(|request| request.object_key),
            Some(key("first-visible"))
        );
        assert_eq!(
            scheduler.dequeue().map(|request| request.object_key),
            Some(key("promoted"))
        );
        assert_eq!(
            scheduler.dequeue().map(|request| request.object_key),
            Some(key("nearby"))
        );
        assert!(scheduler.dequeue().is_none());
    }

    #[test]
    fn bounded_priority_queue_rejects_excess_without_tracking_it() {
        let mut scheduler = ThumbnailScheduler::new(4);
        for index in 0..PRIORITY_QUEUE_CAPACITY {
            assert!(scheduler.enqueue(request(
                &format!("queued-{index}"),
                RequestPriority::Background
            )));
        }

        assert!(!scheduler.enqueue(request("excess", RequestPriority::Background)));
        assert_eq!(scheduler.pending_count(), PRIORITY_QUEUE_CAPACITY);
        let dequeued = std::iter::from_fn(|| scheduler.dequeue())
            .map(|request| request.object_key)
            .collect::<Vec<_>>();
        assert_eq!(dequeued.len(), PRIORITY_QUEUE_CAPACITY);
        assert!(!dequeued.contains(&key("excess")));
        assert_eq!(scheduler.pending_count(), 0);
    }

    #[test]
    fn failed_priority_promotion_preserves_original_request() {
        let mut scheduler = ThumbnailScheduler::new(4);
        for index in 0..PRIORITY_QUEUE_CAPACITY {
            assert!(scheduler.enqueue(request(
                &format!("visible-{index}"),
                RequestPriority::Visible
            )));
        }
        assert!(scheduler.enqueue(request("promoted", RequestPriority::Background)));

        assert!(!scheduler.enqueue(request("promoted", RequestPriority::Visible)));
        assert_eq!(scheduler.pending_count(), PRIORITY_QUEUE_CAPACITY + 1);
        let dequeued = std::iter::from_fn(|| scheduler.dequeue())
            .map(|request| request.object_key)
            .collect::<Vec<_>>();
        assert_eq!(dequeued.last(), Some(&key("promoted")));
    }
}
