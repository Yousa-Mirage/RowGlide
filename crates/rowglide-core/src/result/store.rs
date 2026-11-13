//! Worker-only reclamation; metadata locks are never held across filesystem operations.

use super::budget::{MemoryCharge, Resources};
use super::{CacheLimits, CacheUsage, CachedBatch, Segment, Shared, State, buffer_bytes, disk};
use crate::QueryError;
use arrow_array::RecordBatch;
use std::{
    io::Seek,
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

/// Shared SQL result cache. Every application result must use the same instance.
/// Memory-only batches spill incrementally under pressure; disk-backed caches are
/// evicted first. Byte budgets exclude in-flight batches and external Arrow clones.
pub struct ResultStore {
    /// Atomic charges allow ownership release without blocking the GUI on a worker.
    resources: Arc<Resources>,
    /// Serializes reclamation, writes and decoding, but is never acquired by GUI reads.
    gate: Mutex<()>,
    /// Weak entries prevent the store from retaining closed tabs; registration is brief.
    results: Mutex<Vec<Weak<Shared>>>,
    /// Logical access clock avoids wall-clock changes affecting eviction order.
    clock: AtomicU64,
}

impl ResultStore {
    /// Validate nonzero byte budgets and create an empty store without filesystem work.
    pub fn new(limits: CacheLimits) -> Result<Arc<Self>, QueryError> {
        Ok(Arc::new(Self {
            resources: Resources::new(limits)?,
            gate: Mutex::new(()),
            results: Mutex::new(Vec::new()),
            clock: AtomicU64::new(0),
        }))
    }

    /// Read current retained-buffer and live-file charges without waiting for workers.
    pub fn usage(&self) -> CacheUsage {
        self.resources.usage()
    }

    /// Allocate a cheap monotonic access stamp for registration or GUI row access.
    pub(super) fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a weak lifetime without waiting for ongoing disk writes or decoding.
    pub(super) fn register(&self, shared: &Arc<Shared>) {
        let mut results = self
            .results
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        results.retain(|result| result.strong_count() > 0);
        results.push(Arc::downgrade(shared));
    }

    /// Serialize storage work; poison recovery leaves normal error checks in force.
    fn lock(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Snapshot live results in least-recently-accessed order without retaining registry locks.
    fn candidates(&self) -> Vec<Arc<Shared>> {
        let mut entries = self
            .results
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut results: Vec<_> = entries.iter().filter_map(Weak::upgrade).collect();
        entries.retain(|entry| entry.strong_count() > 0);
        results.sort_by_key(|result| result.touched.load(Ordering::Relaxed));
        results
    }

    /// Remove an owned cache entry and update both local and global memory accounting.
    fn evict(state: &mut State, index: usize) {
        if let Some(cached) = state.cache.remove(&index) {
            state.snapshot.memory_bytes -= cached.charge.bytes;
        }
    }

    /// Reclaim reloadable caches first, then spill one memory-only batch at a time.
    /// The requester may be cancelled between writes; completed victims remain spillable.
    fn reserve(
        &self,
        requester: &Shared,
        bytes: usize,
        generating: bool,
    ) -> Result<MemoryCharge, QueryError> {
        if bytes > requester.limits.memory_bytes || bytes > self.resources.limits.memory_bytes {
            return Err(disk::error(
                "A batch exceeds the memory cache budget; select smaller values",
            ));
        }
        let candidates = self.candidates();
        loop {
            requester.shutdown.check()?;
            if generating {
                requester.cancel.check()?;
            }
            let local_fits =
                requester.state().snapshot.memory_bytes <= requester.limits.memory_bytes - bytes;
            if local_fits && let Some(charge) = self.resources.reserve_memory(bytes) {
                return Ok(charge);
            }
            // A per-result ceiling can only be relieved by reclaiming that result.
            let eligible = candidates
                .iter()
                .filter(|candidate| local_fits || std::ptr::eq(candidate.as_ref(), requester));
            let mut reclaimed = false;
            for candidate in eligible.clone() {
                let mut state = candidate.state();
                if let Some(position) = state
                    .cache
                    .iter()
                    .filter(|(id, _)| state.segments[**id].persisted)
                    .min_by_key(|(_, cached)| cached.touched)
                    .map(|(id, _)| *id)
                {
                    Self::evict(&mut state, position);
                    reclaimed = true;
                    break;
                }
            }
            if reclaimed {
                continue;
            }
            for candidate in eligible {
                let entry = {
                    let state = candidate.state();
                    if state.spill_failed {
                        None
                    } else {
                        state
                            .cache
                            .iter()
                            .min_by_key(|(_, cached)| cached.touched)
                            .map(|(index, cached)| (*index, cached.batch.clone()))
                    }
                };
                if let Some((index, batch)) = entry {
                    let (offset, length) = self.persist(candidate, &batch)?;
                    let mut state = candidate.state();
                    state.segments[index].persisted = true;
                    state.segments[index].offset = offset;
                    state.segments[index].length = length;
                    state.disk_mode = true;
                    Self::evict(&mut state, index);
                    drop(state);
                    candidate.notify();
                    reclaimed = true;
                    break;
                }
            }
            if !reclaimed {
                return Err(disk::error(
                    "Cannot reclaim SQL result memory; release results or narrow the query",
                ));
            }
        }
    }

    /// Lazily create storage and append exactly one batch with no metadata lock held.
    /// A failed write leaves its partial bytes charged and disables further appends.
    fn persist(&self, shared: &Shared, batch: &RecordBatch) -> Result<(u64, u64), QueryError> {
        let result = self.write_segment(shared, batch);
        if result.is_err() {
            shared.state().spill_failed = true;
        }
        result
    }

    /// Append a bounded IPC stream, publishing disk ownership before any file writes.
    fn write_segment(
        &self,
        shared: &Shared,
        batch: &RecordBatch,
    ) -> Result<(u64, u64), QueryError> {
        if shared.state().spill_failed {
            return Err(disk::error("Previous spill failed"));
        }
        let mut writer = shared
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if writer.is_none() {
            let (disk, file) = disk::Disk::create(self.resources.clone())?;
            shared.state().disk = Some(disk.clone());
            *writer = Some((file, disk));
        }
        let (file, disk) = writer
            .as_mut()
            .ok_or_else(|| disk::error("Missing cache writer"))?;
        let offset = file.stream_position().map_err(QueryError::CacheIo)?;
        let length = disk::append(
            file,
            disk,
            batch,
            shared.limits.disk_bytes.saturating_sub(offset),
        )?;
        shared.state().snapshot.disk_bytes += length;
        Ok((offset, length))
    }

    /// Retain a produced batch or append directly after this result has begun spilling.
    pub(super) fn append(&self, shared: &Shared, batch: RecordBatch) -> Result<(), QueryError> {
        let _gate = self.lock();
        shared.cancel.check()?;
        let bytes = buffer_bytes(&batch);
        if bytes > shared.limits.memory_bytes || bytes > self.resources.limits.memory_bytes {
            return Err(disk::error(
                "A batch exceeds the memory cache budget; select smaller values",
            ));
        }
        let (index, start) = {
            let state = shared.state();
            (state.segments.len(), state.snapshot.rows)
        };
        if index >= shared.limits.batches {
            return Err(disk::error(
                "Result exceeds the batch index budget; narrow the query or add LIMIT",
            ));
        }
        let rows = start
            .checked_add(batch.num_rows())
            .ok_or_else(|| disk::error("Result row count overflow"))?;
        let charge = if shared.state().disk_mode {
            None
        } else {
            Some(self.reserve(shared, bytes, true)?)
        };
        // Reclamation may have moved this very result into disk mode.
        let persisted = shared.state().disk_mode;
        let (offset, length) = if persisted {
            self.persist(shared, &batch)?
        } else {
            (0, 0)
        };
        let mut state = shared.state();
        state.segments.push(Segment {
            start,
            rows: batch.num_rows(),
            persisted,
            offset,
            length,
        });
        state.snapshot.rows = rows;
        if !persisted && let Some(charge) = charge {
            state.snapshot.memory_bytes += charge.bytes;
            state.cache.insert(
                index,
                CachedBatch {
                    batch,
                    charge,
                    touched: self.tick(),
                },
            );
        }
        Ok(())
    }

    /// Decode one missing segment and reserve cache ownership under the worker gate.
    /// Execution cancellation does not prevent browsing retained partial results.
    pub(super) fn read(&self, shared: &Shared, index: usize) -> Result<(), QueryError> {
        let _gate = self.lock();
        shared.shutdown.check()?;
        let source = {
            let state = shared.state();
            if state.cache.contains_key(&index) {
                return Ok(());
            }
            state.disk.clone().zip(state.segments.get(index).copied())
        };
        let Some((disk, segment)) = source else {
            return Ok(());
        };
        if !segment.persisted {
            return Ok(());
        }
        let batch = disk.read(segment.offset, segment.length)?;
        let charge = self.reserve(shared, buffer_bytes(&batch), false)?;
        let mut state = shared.state();
        // Bound hot decoded data per result even when the global budget is large.
        let hot_limit = (64 * 1024 * 1024).max(charge.bytes);
        while state.snapshot.memory_bytes.saturating_add(charge.bytes) > hot_limit {
            let Some(position) = state
                .cache
                .iter()
                .filter(|(id, _)| state.segments[**id].persisted)
                .min_by_key(|(_, cached)| cached.touched)
                .map(|(id, _)| *id)
            else {
                break;
            };
            Self::evict(&mut state, position);
        }
        state.snapshot.memory_bytes += charge.bytes;
        state.cache.insert(
            index,
            CachedBatch {
                batch,
                charge,
                touched: self.tick(),
            },
        );
        Ok(())
    }
}
