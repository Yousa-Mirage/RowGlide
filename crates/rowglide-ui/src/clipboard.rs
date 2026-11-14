//! Immutable copy targets and menus shared by data cells and detail panels.

use crate::table::GridEvents;
use arrow_array::ArrayRef;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::*;
use rowglide_core::{Cancellation, CopyError, CopyFormat, QueryResult};
use std::sync::Arc;

/// Original data captured before opening a menu; independent of the active tab.
#[derive(Clone)]
pub(crate) enum CopyTarget {
    /// Retained Arrow buffers and an array-local row, without copying their contents.
    Cell(ArrayRef, usize),
    /// Retained SQL result plus result-relative row and schema column indices.
    Sql(Arc<QueryResult>, usize, usize),
}

impl CopyTarget {
    /// Resolve the captured original value, awaiting a bounded SQL cache reply when needed.
    /// Cancellation ends this wait without cancelling the query or other copy requests.
    pub(crate) async fn resolve(
        &self,
        cancel: &Cancellation,
    ) -> Result<(ArrayRef, usize), CopyError> {
        cancel.check()?;
        match self {
            Self::Cell(array, row) => Ok((array.clone(), *row)),
            Self::Sql(result, row, col) => {
                let (batch, local) = result.row_async(*row, cancel).await?;
                let array = batch
                    .columns()
                    .get(*col)
                    .ok_or_else(|| CopyError::Invalid("Copy column is unavailable".into()))?;
                Ok((array.clone(), local))
            }
        }
    }
}

/// Explicit copy request carrying the original target and chosen serialization format.
#[derive(Clone)]
pub(crate) struct CopyRequested {
    /// Captured data, retained through asynchronous cache reads and serialization.
    pub(crate) target: CopyTarget,
    /// Raw scalar text or JSON output chosen by the user.
    pub(crate) format: CopyFormat,
}

impl EventEmitter<CopyRequested> for GridEvents {}

/// Add copy actions with owned targets, so later tab/query changes cannot redirect them.
pub(crate) fn copy_menu(
    mut menu: PopupMenu,
    target: Option<CopyTarget>,
    events: Entity<GridEvents>,
) -> PopupMenu {
    for (label, format) in [
        ("Copy value", CopyFormat::Value),
        ("Copy as JSON", CopyFormat::Json),
    ] {
        let target = target.clone();
        let events = events.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .disabled(target.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(target) = target.clone() {
                        events.update(cx, |_, cx| cx.emit(CopyRequested { target, format }));
                    }
                }),
        );
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::CopyTarget;
    use arrow_array::{ArrayRef, StringArray};
    use rowglide_core::{Cancellation, CopyFormat, CopyLimits, serialize_cell};
    use std::sync::Arc;

    /// A menu retains its original array even after the visible result is replaced.
    #[test]
    fn target_retains_original_data() -> Result<(), Box<dyn std::error::Error>> {
        let mut active = Arc::new(StringArray::from(vec!["original"])) as ArrayRef;
        let target = CopyTarget::Cell(active.clone(), 0);
        active = Arc::new(StringArray::from(vec!["replacement"]));
        let (array, row) =
            futures_lite::future::block_on(target.resolve(&Cancellation::default()))?;
        assert_eq!(
            serialize_cell(
                array.as_ref(),
                row,
                CopyFormat::Value,
                CopyLimits::default(),
                &Cancellation::default()
            )?,
            "original"
        );
        assert!(!Arc::ptr_eq(&array, &active));
        Ok(())
    }
}

#[cfg(test)]
mod sql_tests {
    use super::CopyTarget;
    use rowglide_core::{
        CacheLimits, Cancellation, CopyFormat, CopyLimits, MockBackend, Query, QueryResult,
        ResultLimits, ResultPhase, ResultStore, serialize_cell,
    };
    use std::{
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    /// Force disk storage and verify copying resolves the original typed SQL cell.
    #[test]
    fn copy_target_rereads_spilled_sql() -> Result<(), Box<dyn std::error::Error>> {
        let store = ResultStore::new(CacheLimits::default())?;
        let result = Arc::new(QueryResult::start(
            store,
            Arc::new(MockBackend),
            Query::default(),
            Cancellation::default(),
            ResultLimits {
                memory_bytes: 128 * 1024,
                ..ResultLimits::default()
            },
        )?);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match result.snapshot().phase {
                ResultPhase::Complete => break,
                ResultPhase::Failed(error) => return Err(error.into()),
                ResultPhase::Cancelled => return Err("Unexpected cancellation".into()),
                ResultPhase::Running => {}
            }
            if Instant::now() > deadline {
                return Err("Result generation timed out".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(result.snapshot().disk_bytes > 0);
        let target = CopyTarget::Sql(result.clone(), 0, 0);
        drop(result);
        let (array, row) =
            futures_lite::future::block_on(target.resolve(&Cancellation::default()))?;
        assert_eq!(
            serialize_cell(
                array.as_ref(),
                row,
                CopyFormat::Value,
                CopyLimits::default(),
                &Cancellation::default()
            )?,
            "10001"
        );
        Ok(())
    }
}
