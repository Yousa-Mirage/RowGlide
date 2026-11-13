//! Explicit browse pages with one-row lookahead and a bounded retained result.

use crate::{Backend, Cancellation, Preview, Query, QueryError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowsePage {
    limit: usize,
    offset: usize,
}

pub struct PageResult {
    pub preview: Preview,
    pub has_next: bool,
}

impl Default for BrowsePage {
    fn default() -> Self {
        Self {
            limit: 1000,
            offset: 0,
        }
    }
}

impl BrowsePage {
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

    pub fn limit(self) -> usize {
        self.limit
    }
    pub fn offset(self) -> usize {
        self.offset
    }

    pub fn previous(self) -> Self {
        Self {
            offset: self.offset.saturating_sub(self.limit),
            ..self
        }
    }

    pub fn next_page(self) -> Result<Self, QueryError> {
        Self::new(self.limit, self.offset + self.limit)
    }

    /// Run the applied browse query and retain exactly one complete page.
    /// Memory exhaustion is an error, never a successful partial page.
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

    struct Fixture {
        batch: RecordBatch,
        dropped: Arc<AtomicBool>,
    }

    struct Cursor {
        batch: Option<RecordBatch>,
        schema: SchemaRef,
        dropped: Arc<AtomicBool>,
    }

    impl Drop for Cursor {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Relaxed);
        }
    }

    impl BatchCursor for Cursor {
        fn schema(&self) -> SchemaRef {
            self.schema.clone()
        }
        fn total_rows(&self) -> Option<usize> {
            None
        }
        fn next_batch(&mut self) -> Result<Option<RecordBatch>, QueryError> {
            Ok(self.batch.take())
        }
    }

    impl Backend for Fixture {
        fn sources(&self) -> Vec<Source> {
            vec![]
        }
        fn query(&self, _: Query, _: Cancellation) -> Result<Box<dyn BatchCursor>, QueryError> {
            Ok(Box::new(Cursor {
                batch: Some(self.batch.clone()),
                schema: self.batch.schema(),
                dropped: self.dropped.clone(),
            }))
        }
    }

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
