//! Background first-batch column sizing with session identity and manual-width protection.

use super::Workspace;
use crate::table::sizing::measure_columns;
use gpui_kit::{component::ActiveTheme, *};
use rowglide_core::{Cancellation, ResultPhase};

/// Owns sizing completion and cancels both cache waiting and CPU measurement on drop.
pub(super) struct SizingTask {
    /// Separate from query cancellation so partial results can still be measured.
    cancel: Cancellation,
    /// Dropping the completion future suppresses publication to an obsolete session.
    _task: Task<()>,
}

impl Drop for SizingTask {
    /// Stop work when switching sessions, closing a file or resizing manually.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Workspace {
    /// Schedule one measurement once a first batch or a terminal empty schema is available.
    /// SQL cache misses use asynchronous reads; shaping runs on the background
    /// executor and retains only the first batch. Session changes drop this task, and
    /// request identity rejects stale completions. A manual resize always takes priority.
    pub(super) fn size_columns(&mut self, cx: &mut Context<Self>) {
        if self.sizing_task.is_some() || self.tabs.is_empty() {
            return;
        }
        let grid = self.table.read(cx).delegate();
        if grid.widths.is_some() {
            return;
        }
        let Some(schema) = grid.schema() else {
            return;
        };
        let sql = grid.sql_result.clone();
        let batch = grid.preview.as_ref().and_then(|preview| {
            preview
                .batches
                .iter()
                .find(|batch| batch.num_rows() > 0)
                .cloned()
        });
        let sql_rows = grid
            .sql_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.rows);
        if sql.is_some()
            && sql_rows == 0
            && grid
                .sql_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.phase == ResultPhase::Running)
        {
            return;
        }
        let sorting = grid.sortable;
        let text_system = cx.text_system().clone();
        let text_font = font(cx.theme().font_family.clone());
        let font_size = cx.theme().font_size * 0.875;
        let color = cx.theme().foreground;
        let request = self.request_key();
        let cancel = Cancellation::default();
        let worker_cancel = cancel.clone();
        let task = cx.spawn(async move |this, cx| {
            let batch = if let Some(sql) = sql.filter(|_| sql_rows > 0) {
                match sql.row_async(0, &worker_cancel).await {
                    Ok((batch, _)) => Some(batch),
                    // A failed cache read leaves usable header widths; query status owns the error.
                    Err(_) => None,
                }
            } else {
                batch
            };
            let Ok(widths) = cx
                .background_executor()
                .spawn(async move {
                    let text_system = WindowTextSystem::new(text_system);
                    measure_columns(&schema, batch.as_ref(), sorting, &worker_cancel, |text| {
                        text.lines()
                            .map(|line| {
                                let run = TextRun {
                                    len: line.len(),
                                    font: text_font.clone(),
                                    color,
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                text_system
                                    .shape_line(line.to_owned().into(), font_size, &[run], None)
                                    .width
                            })
                            .max()
                            .unwrap_or(px(0.))
                    })
                })
                .await
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if request != this.request_key() || this.table.read(cx).delegate().widths.is_some()
                {
                    return;
                }
                this.active_tab_mut().result_mut().widths = Some(widths.clone());
                this.table.update(cx, |table, cx| {
                    table.delegate_mut().widths = Some(widths);
                    table.refresh(cx);
                });
                cx.notify();
            });
        });
        self.sizing_task = Some(SizingTask {
            cancel,
            _task: task,
        });
    }
}
