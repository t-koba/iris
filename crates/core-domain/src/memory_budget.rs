use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, OnceLock,
};

use tokio::sync::Notify;

pub const DEFAULT_MEMORY_BUDGET_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug)]
pub struct MemoryBudget {
    allocated: AtomicUsize,
    cap: AtomicUsize,
    available: Notify,
}

impl MemoryBudget {
    pub fn global() -> Arc<Self> {
        static INSTANCE: OnceLock<Arc<MemoryBudget>> = OnceLock::new();
        INSTANCE
            .get_or_init(|| Arc::new(Self::new(DEFAULT_MEMORY_BUDGET_BYTES)))
            .clone()
    }

    pub fn new(cap: usize) -> Self {
        Self {
            allocated: AtomicUsize::new(0),
            cap: AtomicUsize::new(cap.max(1)),
            available: Notify::new(),
        }
    }

    pub fn cap_bytes(&self) -> usize {
        self.cap.load(Ordering::Acquire)
    }

    pub fn set_cap_bytes(&self, cap: usize) {
        self.cap.store(cap.max(1), Ordering::Release);
        self.available.notify_waiters();
    }

    pub fn allocated_bytes(&self) -> usize {
        self.allocated.load(Ordering::Relaxed)
    }

    pub async fn acquire(self: &Arc<Self>, bytes: usize) -> Option<MemoryBudgetPermit> {
        let bytes = bytes.max(1);
        loop {
            let notified = self.available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if bytes > self.cap_bytes() {
                return None;
            }
            if self.reserve(bytes) {
                return Some(MemoryBudgetPermit {
                    budget: self.clone(),
                    bytes,
                });
            }
            notified.await;
        }
    }

    pub fn try_acquire(self: &Arc<Self>, bytes: usize) -> Option<MemoryBudgetPermit> {
        let bytes = bytes.max(1);
        if bytes > self.cap_bytes() || !self.reserve(bytes) {
            return None;
        }
        Some(MemoryBudgetPermit {
            budget: self.clone(),
            bytes,
        })
    }

    fn reserve(&self, bytes: usize) -> bool {
        let mut allocated = self.allocated.load(Ordering::Acquire);
        loop {
            let cap = self.cap_bytes();
            let Some(next) = allocated.checked_add(bytes) else {
                return false;
            };
            if next > cap {
                return false;
            }
            match self.allocated.compare_exchange_weak(
                allocated,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(current) => allocated = current,
            }
        }
    }
}

#[derive(Debug)]
pub struct MemoryBudgetPermit {
    budget: Arc<MemoryBudget>,
    bytes: usize,
}

impl Drop for MemoryBudgetPermit {
    fn drop(&mut self) {
        self.budget
            .allocated
            .fetch_sub(self.bytes, Ordering::Relaxed);
        self.budget.available.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn releases_allocation_on_drop() {
        let budget = Arc::new(MemoryBudget::new(256 * 1024));
        assert_eq!(budget.allocated_bytes(), 0);
        assert_eq!(budget.cap_bytes(), 256 * 1024);

        let permit = budget.acquire(96 * 1024).await.expect("memory permit");
        assert_eq!(budget.allocated_bytes(), 96 * 1024);

        drop(permit);
        assert_eq!(budget.allocated_bytes(), 0);
        assert!(budget.acquire(512 * 1024).await.is_none());
    }

    #[tokio::test]
    async fn zero_byte_requests_hold_one_byte_of_accounting() {
        let budget = Arc::new(MemoryBudget::new(64 * 1024));
        let permit = budget.acquire(0).await.expect("memory permit");

        assert_eq!(budget.allocated_bytes(), 1);
        drop(permit);
        assert_eq!(budget.allocated_bytes(), 0);
    }

    #[test]
    fn try_acquire_respects_and_releases_shared_capacity() {
        let budget = Arc::new(MemoryBudget::new(128 * 1024));
        let first = budget.try_acquire(96 * 1024).expect("first permit");
        assert!(budget.try_acquire(64 * 1024).is_none());
        drop(first);
        let second = budget.try_acquire(64 * 1024).expect("released capacity");
        assert_eq!(budget.allocated_bytes(), 64 * 1024);
        drop(second);
    }

    #[tokio::test]
    async fn release_wakes_smaller_waiter_while_oversized_waiter_stays_blocked() {
        let budget = Arc::new(MemoryBudget::new(100));
        let headroom = budget.acquire(60).await.expect("headroom permit");
        let releasable = budget.acquire(30).await.expect("releasable permit");

        let oversized = tokio::spawn({
            let budget = Arc::clone(&budget);
            async move { budget.acquire(50).await }
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        let small = tokio::spawn({
            let budget = Arc::clone(&budget);
            async move { budget.acquire(20).await }
        });
        tokio::time::sleep(Duration::from_millis(25)).await;

        drop(releasable);
        let small_permit = tokio::time::timeout(Duration::from_millis(300), small)
            .await
            .expect("small waiter wakes after release")
            .expect("small waiter joins")
            .expect("small permit");
        assert_eq!(budget.allocated_bytes(), 80);
        assert!(!oversized.is_finished());

        drop(small_permit);
        drop(headroom);
        let oversized_permit = tokio::time::timeout(Duration::from_millis(300), oversized)
            .await
            .expect("oversized waiter wakes after headroom is released")
            .expect("oversized waiter joins")
            .expect("oversized permit");
        drop(oversized_permit);
        assert_eq!(budget.allocated_bytes(), 0);
    }

    #[tokio::test]
    async fn shrinking_cap_blocks_new_allocations_until_existing_permits_drop() {
        let budget = Arc::new(MemoryBudget::new(128 * 1024));
        let existing = budget.acquire(96 * 1024).await.expect("existing permit");
        budget.set_cap_bytes(64 * 1024);

        let pending = tokio::spawn({
            let budget = Arc::clone(&budget);
            async move { budget.acquire(64 * 1024).await }
        });
        tokio::task::yield_now().await;
        assert!(!pending.is_finished());

        drop(existing);
        let acquired = pending.await.expect("join").expect("permit after release");
        assert_eq!(budget.cap_bytes(), 64 * 1024);
        drop(acquired);
    }
}
