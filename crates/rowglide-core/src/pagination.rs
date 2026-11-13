//! Explicit browse pages with one-row lookahead and a bounded retained result.

use crate::{Backend, Cancellation, Preview, Query, QueryError};

/// Validated Browse range with a positive page size and a zero-based offset.
///
/// The private fields ensure that the range plus one lookahead row fits a signed
/// 64-bit integer. Navigation returns new ranges; it neither fetches data nor
/// changes the source query until `Self::load` is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowsePage {
    /// Positive number of rows to display, excluding the lookahead row.
    limit: usize,
    /// Zero-based position in the globally filtered and sorted result.
    offset: usize,
}

/// A complete retained Browse page and evidence that another row exists.
///
/// A short or empty page is valid at EOF. Memory-budget failures return an error
/// instead of constructing a partial `PageResult`.
pub struct PageResult {
    /// Complete requested page or the shorter final page, with its result schema.
    pub preview: Preview,
    /// True only when an extra row beyond this page was actually fetched.
    pub has_next: bool,
}

impl Default for BrowsePage {
    /// Select the first page with a 1,000-row limit and an offset of zero.
    fn default() -> Self {
        Self {
            limit: 1000,
            offset: 0,
        }
    }
}

impl BrowsePage {
    /// Validate a page size and zero-based starting offset.
    ///
    /// # Errors
    ///
    /// Rejects zero limits, arithmetic overflow, and ranges that cannot fit in a signed
    /// 64-bit integer with one additional lookahead row.
    pub fn new(limit: usize, offset: usize) -> Result<Self, QueryError> {
        // Reserve one extra row to detect the next page without a COUNT scan.
        if limit == 0
            || offset
                .checked_add(limit)
                .and_then(|end| end.checked_add(1))
                .and_then(|end| i64::try_from(end).ok())
                .is_none()
        {
            return Err(QueryError::Invalid(
                "Limit must be positive; Offset and the page range must fit a signed 64-bit integer".into(),
            ));
        }
        Ok(Self { limit, offset })
    }

    /// Return the maximum number of rows displayed on this page, excluding lookahead.
    pub fn limit(self) -> usize {
        self.limit
    }
    /// Return how many rows to skip after filtering and sorting the source.
    pub fn offset(self) -> usize {
        self.offset
    }

    /// Move back by one page size, clamping at offset zero.
    ///
    /// A manually entered offset need not align with the page size; navigating backward
    /// from such an offset preserves that relative position until the first page.
    pub fn previous(self) -> Self {
        Self {
            offset: self.offset.saturating_sub(self.limit),
            ..self
        }
    }

    /// Move forward by one page size while preserving the limit.
    ///
    /// This only validates arithmetic; callers must use `PageResult::has_next` to decide
    /// whether another page exists. Returns an error if the new range is too large.
    pub fn next_page(self) -> Result<Self, QueryError> {
        Self::new(self.limit, self.offset + self.limit)
    }

    /// Execute a Browse query and collect one complete page on the calling thread.
    ///
    /// Replaces the query's range with this range plus one lookahead row, preserving
    /// its source, filter and sort. The lookahead is excluded from displayed rows and
    /// sets `PageResult::has_next`, avoiding a full count scan. Retained Arrow buffers
    /// are capped at 16 MiB, including buffers shared by sliced batches. The cursor is
    /// dropped before returning, which may wait for backend cleanup.
    ///
    /// # Errors
    ///
    /// Rejects SQL requests and pages exceeding the memory budget. Propagates backend,
    /// fetch and cancellation errors; no partial page is returned as a success.
    pub fn load(
        self,
        backend: &dyn Backend,
        mut query: Query,
        cancel: &Cancellation,
    ) -> Result<PageResult, QueryError> {
        const MAX_BYTES: usize = 16 * 1024 * 1024;

        if query.sql.is_some() {
            return Err(QueryError::Invalid(
                "Browse pagination cannot wrap SQL queries".into(),
            ));
        }
        query.limit = Some(self.limit + 1);
        query.offset = self.offset;
        let mut cursor = backend.query(query, cancel.clone())?;
        let mut preview = Preview {
            schema: cursor.schema(),
            batches: vec![],
            rows: 0,
            total_rows: None,
            truncated: false,
        };
        let mut bytes = 0usize;
        let mut has_next = false;
        loop {
            cancel.check()?;
            let Some(batch) = cursor.next_batch()? else {
                break;
            };
            let keep = batch.num_rows().min(self.limit - preview.rows);
            if keep > 0 {
                let retained = batch.slice(0, keep);
                // Slices may retain the original buffers; charge those too.
                bytes = bytes.saturating_add(retained.get_array_memory_size());
                if bytes > MAX_BYTES {
                    return Err(QueryError::Invalid(
                        "Page exceeds the 16 MiB memory budget; reduce Limit and try again".into(),
                    ));
                }
                preview.rows += keep;
                preview.batches.push(retained);
            }
            if batch.num_rows() > keep {
                has_next = true;
                break;
            }
        }
        cancel.check()?;
        Ok(PageResult { preview, has_next })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BatchCursor, Source};
    use arrow_array::{ArrayRef, RecordBatch, StringArray};
    use arrow_schema::SchemaRef;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    /// Test backend that supplies one controlled batch and tracks cursor destruction.
    ///
    /// It deliberately ignores query ranges and cancellation so collector behavior can
    /// be tested independently of a real engine.
    struct Fixture {
        /// Controlled Arrow batch cloned into each test cursor.
        batch: RecordBatch,
        /// Shared flag set when a test cursor is destroyed.
        dropped: Arc<AtomicBool>,
    }

    /// Single-batch test cursor with independently retained schema and a drop flag.
    struct Cursor {
        /// Single pending batch, moved out on the first pull.
        batch: Option<RecordBatch>,
        /// Schema that remains available after the pending batch is consumed.
        schema: SchemaRef,
        /// Shared lifetime probe used by resource-release assertions.
        dropped: Arc<AtomicBool>,
    }

    impl Drop for Cursor {
        /// Record cursor destruction so failure-path tests can verify resource release.
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Relaxed);
        }
    }

    impl BatchCursor for Cursor {
        /// Return the fixture schema even after its only batch has been consumed.
        fn schema(&self) -> SchemaRef {
            self.schema.clone()
        }
        /// Report an unknown total to exercise collection without count metadata.
        fn total_rows(&self) -> Option<usize> {
            None
        }
        /// Move out the sole fixture batch, returning EOF on every subsequent call.
        fn next_batch(&mut self) -> Result<Option<RecordBatch>, QueryError> {
            Ok(self.batch.take())
        }
    }

    impl Backend for Fixture {
        /// Return no registered sources; this fixture serves batches directly.
        fn sources(&self) -> Vec<Source> {
            vec![]
        }
        /// Create an independent single-batch cursor sharing the fixture's drop flag.
        fn query(&self, _: Query, _: Cancellation) -> Result<Box<dyn BatchCursor>, QueryError> {
            Ok(Box::new(Cursor {
                batch: Some(self.batch.clone()),
                schema: self.batch.schema(),
                dropped: self.dropped.clone(),
            }))
        }
    }

    /// Check positive limits, overflow rejection and navigation from unaligned offsets.
    ///
    /// The boundary case verifies that a valid current page can still reject its next
    /// range when the signed 64-bit lookahead limit would be exceeded.
    #[test]
    fn page_ranges_validate_and_navigate() -> Result<(), QueryError> {
        assert!(BrowsePage::new(0, 0).is_err());
        assert!(BrowsePage::new(usize::MAX, 0).is_err());
        assert!(BrowsePage::new(1, usize::MAX).is_err());
        let page = BrowsePage::new(1000, 1500)?;
        assert_eq!(page.previous().offset(), 500);
        assert_eq!(page.previous().previous().offset(), 0);
        assert_eq!(page.next_page()?.offset(), 2500);
        let last = BrowsePage::new(1, i64::MAX as usize - 2)?;
        assert!(last.next_page().is_err());
        Ok(())
    }

    /// Verify that memory-budget and cancellation failures both destroy the cursor.
    ///
    /// An oversized string batch must produce an explicit reduce-Limit error instead
    /// of silently returning a truncated page.
    #[test]
    fn oversized_page_and_cancellation_release_the_cursor() -> Result<(), Box<dyn std::error::Error>>
    {
        let values: ArrayRef = Arc::new(StringArray::from(vec!["x".repeat(17 * 1024 * 1024)]));
        let backend = Fixture {
            batch: RecordBatch::try_from_iter([("value", values)])?,
            dropped: Arc::new(AtomicBool::new(false)),
        };
        let result =
            BrowsePage::default().load(&backend, Query::default(), &Cancellation::default());
        assert!(
            matches!(result, Err(QueryError::Invalid(message)) if message.contains("reduce Limit"))
        );
        assert!(backend.dropped.load(Ordering::Relaxed));

        backend.dropped.store(false, Ordering::Relaxed);
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(matches!(
            BrowsePage::default().load(&backend, Query::default(), &cancel),
            Err(QueryError::Cancelled)
        ));
        assert!(backend.dropped.load(Ordering::Relaxed));
        Ok(())
    }
}
