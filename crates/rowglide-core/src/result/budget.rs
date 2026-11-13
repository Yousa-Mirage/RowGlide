//! Shared resource accounting independent of query execution and GUI lifetimes.

use crate::QueryError;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

/// Global SQL result-cache budgets shared by every tab in one application instance.
/// These exclude Browse pages, engine allocations, in-flight batches and inspector references.
#[derive(Clone, Copy, Debug)]
pub struct CacheLimits {
    /// Maximum retained Arrow buffer bytes across all SQL results and decoded caches.
    pub memory_bytes: usize,
    /// Maximum live IPC bytes, including incomplete writes and files pending deletion.
    /// Failed deletion keeps its charge until the manager is discarded or the process exits.
    pub disk_bytes: u64,
}

impl CacheLimits {
    /// Use a quarter of detected RAM capped at 4 GiB, and 32 GiB of result disk storage.
    /// Missing/zero RAM information falls back to a conservative 512 MiB memory budget.
    pub fn for_total_memory(total: Option<u64>) -> Self {
        let bytes = total
            .filter(|bytes| *bytes > 0)
            .map_or(512 * 1024 * 1024, |bytes| {
                (bytes / 4).clamp(1, 4 * 1024 * 1024 * 1024)
            });
        Self {
            memory_bytes: usize::try_from(bytes).unwrap_or(usize::MAX),
            disk_bytes: 32 * 1024 * 1024 * 1024,
        }
    }
}

impl Default for CacheLimits {
    /// Use conservative limits when the embedding application has not detected system RAM.
    fn default() -> Self {
        Self::for_total_memory(None)
    }
}

/// Current global cache charges; neither field represents whole-process memory or disk use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheUsage {
    /// Bytes retained by SQL result caches, excluding borrowed GUI/inspector arrays.
    pub memory_bytes: usize,
    /// IPC file bytes still owned or awaiting successful background deletion.
    pub disk_bytes: u64,
}

/// Atomic counters allow cache/file release without acquiring the worker coordination lock.
pub(super) struct Resources {
    /// Immutable application-wide limits.
    pub(super) limits: CacheLimits,
    /// Cache ownership charges; reservations are refunded when their guards are dropped.
    memory: AtomicUsize,
    /// Actual IPC writes; refunded only after truncation or successful file deletion.
    disk: AtomicU64,
}

impl Resources {
    /// Validate nonzero budgets before creating shared counters.
    pub(super) fn new(limits: CacheLimits) -> Result<Arc<Self>, QueryError> {
        if limits.memory_bytes == 0 || limits.disk_bytes == 0 {
            return Err(QueryError::Invalid("Cache budgets must be positive".into()));
        }
        Ok(Arc::new(Self {
            limits,
            memory: AtomicUsize::new(0),
            disk: AtomicU64::new(0),
        }))
    }

    /// Reserve retained buffer bytes atomically, returning None when reclamation is needed.
    pub(super) fn reserve_memory(self: &Arc<Self>, bytes: usize) -> Option<MemoryCharge> {
        self.memory
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.limits.memory_bytes)
            })
            .ok()?;
        Some(MemoryCharge {
            resources: self.clone(),
            bytes,
        })
    }

    /// Reserve a disk write before issuing it so concurrent resources cannot overcommit.
    pub(super) fn reserve_disk(&self, bytes: u64) -> Result<(), QueryError> {
        self.disk.try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            used.checked_add(bytes).filter(|next| *next <= self.limits.disk_bytes)
        }).map(|_| ()).map_err(|_| QueryError::Invalid(
            "SQL results exceed the global disk cache budget; release results or narrow the query".into(),
        ))
    }

    /// Refund a failed/unwritten reservation or bytes whose file was successfully removed.
    pub(super) fn release_disk(&self, bytes: u64) {
        self.disk.fetch_sub(bytes, Ordering::AcqRel);
    }

    /// Read lightweight resource counters without waiting for disk work.
    pub(super) fn usage(&self) -> CacheUsage {
        CacheUsage {
            memory_bytes: self.memory.load(Ordering::Acquire),
            disk_bytes: self.disk.load(Ordering::Acquire),
        }
    }
}

/// One cache entry's reservation; moving or dropping entries preserves global accounting.
pub(super) struct MemoryCharge {
    /// Shared counters outlive individual results.
    resources: Arc<Resources>,
    /// Charged retained bytes, computed once when a batch enters the cache.
    pub(super) bytes: usize,
}

impl Drop for MemoryCharge {
    /// Refund cache ownership even during cancellation, failed insertion or result eviction.
    fn drop(&mut self) {
        self.resources
            .memory
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheLimits, Resources};
    use crate::QueryError;
    use std::{sync::Barrier, thread};

    /// Default policy scales with RAM, caps at 4 GiB and falls back when detection fails.
    #[test]
    fn automatic_limits_scale_and_cap() {
        assert_eq!(
            CacheLimits::for_total_memory(Some(8 << 30)).memory_bytes,
            2 << 30
        );
        assert_eq!(
            CacheLimits::for_total_memory(Some(64 << 30)).memory_bytes as u64,
            4 << 30
        );
        assert_eq!(
            CacheLimits::for_total_memory(Some(0)).memory_bytes,
            512 << 20
        );
    }

    /// Competing workers cannot both reserve the last bytes, and dropping ownership refunds them.
    #[test]
    fn reservations_are_atomic_and_released() -> Result<(), QueryError> {
        let resources = Resources::new(CacheLimits {
            memory_bytes: 100,
            disk_bytes: 100,
        })?;
        let barrier = std::sync::Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let resources = resources.clone();
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                let charge = resources.reserve_memory(80);
                barrier.wait();
                barrier.wait();
                charge
            }));
        }
        barrier.wait();
        assert_eq!(resources.usage().memory_bytes, 80);
        barrier.wait();
        for worker in workers {
            drop(
                worker
                    .join()
                    .map_err(|_| QueryError::Backend("Budget worker panicked".into()))?,
            );
        }
        assert_eq!(resources.usage().memory_bytes, 0);
        resources.reserve_disk(90)?;
        assert!(resources.reserve_disk(20).is_err());
        resources.release_disk(90);
        assert_eq!(resources.usage().disk_bytes, 0);
        Ok(())
    }
}
