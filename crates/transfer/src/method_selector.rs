use iris_core_domain::types::capability::CapabilitySet;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const GLOBAL_IN_FLIGHT_REQUEST_BUDGET: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyMethod {
    /// reflink / copy-on-write (APFS, Btrfs, XFS).
    CloneCopy,
    /// Copy done entirely on the server (S3 CopyObject, SMB server-side copy, ...).
    ServerSideCopy,
    /// The client reads the source and writes the destination.
    ClientRelayCopy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveMethod {
    /// rename(2) / MoveFile within one file system.
    RenameMove,
    /// ServerSideCopy + Verify + DeleteSource
    ServerSideCopyThenDelete,
    /// ClientRelayCopy + Verify + DeleteSource
    ClientRelayThenDelete,
}

#[derive(Clone, Copy, Debug)]
struct BackendGovernorState {
    ewma_latency_ms: f64,
    ewma_bytes_per_second: f64,
}

/// Adjusts concurrency per backend from observed results.
#[derive(Debug)]
pub struct ConcurrencyGovernor {
    alpha: f64,
    target_latency_ms: f64,
    min: usize,
    max: usize,
    states: Mutex<HashMap<String, BackendGovernorState>>,
    in_flight: Arc<Semaphore>,
}

impl Default for ConcurrencyGovernor {
    fn default() -> Self {
        Self {
            alpha: 0.3,
            target_latency_ms: 200.0,
            min: 1,
            max: 16,
            states: Mutex::new(HashMap::new()),
            in_flight: Arc::new(Semaphore::new(GLOBAL_IN_FLIGHT_REQUEST_BUDGET)),
        }
    }
}

impl ConcurrencyGovernor {
    pub fn observe(&self, backend_type: &str, latency: Duration, bytes_transferred: u64) -> usize {
        let sample_latency_ms = latency.as_secs_f64().max(0.001) * 1000.0;
        let sample_bps = bytes_transferred as f64 / latency.as_secs_f64().max(0.001);
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let state = states
            .entry(backend_type.to_string())
            .or_insert(BackendGovernorState {
                ewma_latency_ms: sample_latency_ms,
                ewma_bytes_per_second: sample_bps,
            });
        state.ewma_latency_ms =
            self.alpha * sample_latency_ms + (1.0 - self.alpha) * state.ewma_latency_ms;
        state.ewma_bytes_per_second =
            self.alpha * sample_bps + (1.0 - self.alpha) * state.ewma_bytes_per_second;
        self.scaled_workers(default_worker_budget(backend_type), *state)
    }

    pub fn recommended_workers(&self, backend_type: &str, baseline: usize) -> usize {
        let states = self
            .states
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(state) = states.get(backend_type).copied() else {
            return baseline.clamp(self.min, self.max);
        };
        self.scaled_workers(baseline, state)
    }

    pub async fn acquire_in_flight(&self, count: usize) -> Option<OwnedSemaphorePermit> {
        let count = count.clamp(1, GLOBAL_IN_FLIGHT_REQUEST_BUDGET) as u32;
        Arc::clone(&self.in_flight)
            .acquire_many_owned(count)
            .await
            .ok()
    }

    fn scaled_workers(&self, baseline: usize, state: BackendGovernorState) -> usize {
        let latency_factor = (self.target_latency_ms / state.ewma_latency_ms).clamp(0.5, 2.0);
        let throughput_factor = if state.ewma_bytes_per_second >= 50.0 * 1024.0 * 1024.0 {
            1.5
        } else if state.ewma_bytes_per_second >= 10.0 * 1024.0 * 1024.0 {
            1.2
        } else if state.ewma_bytes_per_second <= 1.0 * 1024.0 * 1024.0 {
            0.75
        } else {
            1.0
        };
        ((baseline as f64 * latency_factor * throughput_factor).round() as usize)
            .clamp(self.min, self.max)
    }
}

pub fn global_concurrency_governor() -> &'static ConcurrencyGovernor {
    static GOVERNOR: OnceLock<ConcurrencyGovernor> = OnceLock::new();
    GOVERNOR.get_or_init(ConcurrencyGovernor::default)
}

pub fn observe_backend_sample(
    backend_type: &str,
    latency: Duration,
    bytes_transferred: u64,
) -> usize {
    global_concurrency_governor().observe(backend_type, latency, bytes_transferred)
}

pub fn recommended_workers_for_backend(backend_type: &str, baseline: usize) -> usize {
    global_concurrency_governor().recommended_workers(backend_type, baseline)
}

fn default_worker_budget(backend_type: &str) -> usize {
    match backend_type {
        "smb" | "nfs" => 6,
        "s3" | "pcloud" | "sftp" => 8,
        _ => 4,
    }
}

/// Preference: CloneCopy, then ServerSideCopy, then ClientRelayCopy.
pub fn select_copy_method(
    src_capabilities: &CapabilitySet,
    _dst_capabilities: &CapabilitySet,
    same_backend: bool,
) -> CopyMethod {
    if same_backend && src_capabilities.can_clone {
        return CopyMethod::CloneCopy;
    }

    if same_backend && src_capabilities.can_server_side_copy {
        return CopyMethod::ServerSideCopy;
    }

    CopyMethod::ClientRelayCopy
}

/// Preference: RenameMove, then server-side copy + verify + delete, then relay copy + verify + delete.
pub fn select_move_method(
    src_capabilities: &CapabilitySet,
    _dst_capabilities: &CapabilitySet,
    same_backend: bool,
) -> MoveMethod {
    if same_backend && src_capabilities.can_rename {
        return MoveMethod::RenameMove;
    }

    if same_backend && src_capabilities.can_server_side_copy {
        return MoveMethod::ServerSideCopyThenDelete;
    }

    MoveMethod::ClientRelayThenDelete
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn global_request_budget_bounds_combined_file_and_chunk_parallelism() {
        let governor = Arc::new(ConcurrencyGovernor::default());
        let first = governor.acquire_in_flight(24).await.expect("first permit");
        let waiting_governor = Arc::clone(&governor);
        let waiting = tokio::spawn(async move { waiting_governor.acquire_in_flight(16).await });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!waiting.is_finished());
        drop(first);
        assert!(tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("permit wake")
            .expect("join")
            .is_some());
    }

    fn local_caps() -> CapabilitySet {
        CapabilitySet {
            can_range_read: true,
            can_range_write: true,
            can_rename: true,
            can_clone: true,
            can_provide_progress: true,
            can_verify_by_checksum: true,
            can_trash: true,
            ..CapabilitySet::default()
        }
    }

    fn s3_caps() -> CapabilitySet {
        CapabilitySet {
            can_range_read: true,
            can_server_side_copy: true,
            can_verify_by_checksum: true,
            can_etag: true,
            ..CapabilitySet::default()
        }
    }

    #[test]
    fn test_local_same_backend_copy_selects_clone() {
        let caps = local_caps();
        let method = select_copy_method(&caps, &caps, true);
        assert_eq!(method, CopyMethod::CloneCopy);
    }

    #[test]
    fn test_s3_same_backend_copy_selects_server_side() {
        let caps = s3_caps();
        let method = select_copy_method(&caps, &caps, true);
        assert_eq!(method, CopyMethod::ServerSideCopy);
    }

    #[test]
    fn test_cross_backend_copy_selects_relay() {
        let method = select_copy_method(&local_caps(), &s3_caps(), false);
        assert_eq!(method, CopyMethod::ClientRelayCopy);
    }

    #[test]
    fn test_local_same_backend_move_selects_rename() {
        let caps = local_caps();
        let method = select_move_method(&caps, &caps, true);
        assert_eq!(method, MoveMethod::RenameMove);
    }

    #[test]
    fn test_s3_same_backend_move_selects_server_side() {
        let caps = s3_caps();
        let method = select_move_method(&caps, &caps, true);
        assert_eq!(method, MoveMethod::ServerSideCopyThenDelete);
    }

    #[test]
    fn test_cross_backend_move_selects_relay() {
        let method = select_move_method(&local_caps(), &s3_caps(), false);
        assert_eq!(method, MoveMethod::ClientRelayThenDelete);
    }

    #[test]
    fn governor_scales_down_when_latency_is_high() {
        let governor = ConcurrencyGovernor::default();
        let backend = "s3";
        governor.observe(backend, Duration::from_millis(900), 4 * 1024 * 1024);
        let workers = governor.recommended_workers(backend, 8);
        assert!(workers < 8, "expected fewer workers, got {workers}");
    }

    #[test]
    fn governor_scales_up_when_backend_is_fast() {
        let governor = ConcurrencyGovernor::default();
        let backend = "smb";
        governor.observe(backend, Duration::from_millis(40), 32 * 1024 * 1024);
        let workers = governor.recommended_workers(backend, 4);
        assert!(workers > 4, "expected more workers, got {workers}");
    }
}
