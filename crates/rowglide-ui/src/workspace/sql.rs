//! SQL result lifecycle and coalesced GUI updates; all disk work remains in core.

use super::{Workspace, state::TabMode};
use gpui_kit::*;
use rowglide_core::{QueryResult, ResultLimits, ResultPhase, ResultSnapshot};
use std::{sync::Arc, time::Duration};

impl Workspace {
    /// Start continuous SQL generation and attach its cache to the active table.
    /// Thread startup errors become visible immediately; execution errors arrive in snapshots.
    pub(super) fn start_sql(&mut self, cx: &mut Context<Self>) {
        match QueryResult::start(
            self.backend.clone(),
            self.query.clone(),
            self.cancellation.clone(),
            ResultLimits::default(),
        ) {
            Ok(result) => {
                self.table.update(cx, |table, cx| {
                    table.delegate_mut().sql_result = Some(Arc::new(result));
                    cx.notify();
                });
                self.watch_sql(cx);
            }
            Err(error) => {
                self.busy = false;
                self.error = true;
                self.status = error.to_string();
                self.table.update(cx, |table, cx| {
                    table.delegate_mut().loading = false;
                    cx.notify();
                });
            }
        }
    }

    /// Observe the active SQL cache through the existing GPUI executor without blocking it.
    /// Request identity rejects old notifications. The observer remains active after EOF
    /// because scrolling can asynchronously recover evicted batches from disk.
    pub(super) fn watch_sql(&mut self, cx: &mut Context<Self>) {
        if self.active_tab().mode != TabMode::Sql {
            return;
        }
        let Some(result) = self.table.read(cx).delegate().sql_result.clone() else {
            return;
        };
        let request = self.request_key();
        self.task = Some(cx.spawn(async move |this, cx| {
            loop {
                let snapshot = result.snapshot();
                let applied = this.update(cx, |this, cx| {
                    if request != this.request_key() {
                        return false;
                    }
                    this.busy = snapshot.phase == ResultPhase::Running;
                    this.error = matches!(snapshot.phase, ResultPhase::Failed(_));
                    this.status = status(&snapshot);
                    this.table.update(cx, |table, cx| {
                        let grid = table.delegate_mut();
                        let first_schema = grid
                            .sql_snapshot
                            .as_ref()
                            .and_then(|s| s.schema.as_ref())
                            .is_none()
                            && snapshot.schema.is_some();
                        // Keep existing rows visible while more batches are produced.
                        grid.loading =
                            snapshot.schema.is_none() && snapshot.phase == ResultPhase::Running;
                        grid.sql_snapshot = Some(snapshot);
                        if first_schema {
                            table.refresh(cx);
                            table.scroll_to_row(0, cx);
                        }
                        cx.notify();
                    });
                    // Finish the original click after its batch arrives, without another click.
                    if let Some((row, col)) = this.pending_cell
                        && let Some((batch, local)) = this.table.read(cx).delegate().row(row)
                    {
                        this.selected = Some((
                            batch.schema().field(col).name().clone(),
                            batch.column(col).clone(),
                            local,
                        ));
                        this.pending_cell = None;
                    }
                    cx.notify();
                    true
                });
                if !matches!(applied, Ok(true)) {
                    break;
                }
                result.changed().await;
                // Coalesce fast batch and cache events to avoid repainting for every batch.
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
            }
        }));
    }
}

/// Describe generated rows without implying completeness before a successful terminal event.
fn status(snapshot: &ResultSnapshot) -> String {
    match &snapshot.phase {
        ResultPhase::Running if snapshot.schema.is_none() => "Executing SQL…".into(),
        ResultPhase::Running => format!("{} rows loaded · Loading…", snapshot.rows),
        ResultPhase::Complete => format!("{} rows · Complete", snapshot.rows),
        ResultPhase::Cancelled => {
            format!("{} rows loaded · Cancelled · Partial result", snapshot.rows)
        }
        ResultPhase::Failed(error) => {
            format!("{} rows loaded · Partial result · {error}", snapshot.rows)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::status;
    use rowglide_core::{ResultPhase, ResultSnapshot};

    /// Completion, cancellation and failure cannot share a misleading success label.
    #[test]
    fn terminal_status_marks_partial_results() {
        let mut snapshot = ResultSnapshot {
            schema: None,
            rows: 12000,
            phase: ResultPhase::Complete,
            memory_bytes: 0,
            disk_bytes: 0,
        };
        assert_eq!(status(&snapshot), "12000 rows · Complete");
        snapshot.phase = ResultPhase::Cancelled;
        assert!(status(&snapshot).contains("Partial result"));
        snapshot.phase = ResultPhase::Failed("Disk full".into());
        assert!(status(&snapshot).contains("Disk full"));
        assert!(!status(&snapshot).contains("Complete"));
    }
}
