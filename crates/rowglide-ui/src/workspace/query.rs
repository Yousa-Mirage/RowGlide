//! Browse input validation and background query lifecycle.

use super::Workspace;
use gpui_kit::*;
use rowglide_app::{Cancellation, Preview};
use std::time::Instant;

impl Workspace {
    // Validate browse controls before replacing the active query.
    pub(super) fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let limit = self.limit.read(cx).value();
        let offset = self.offset.read(cx).value();
        let parse = |value: &str| value.trim().parse::<usize>();
        let limit = if limit.trim().is_empty() {
            Ok(None)
        } else {
            parse(&limit).map(Some)
        };
        let offset = if offset.trim().is_empty() {
            Ok(0)
        } else {
            parse(&offset)
        };

        match (limit, offset) {
            (Ok(limit), Ok(offset)) => {
                self.query.limit = limit;
                self.query.offset = offset;
                self.query.search = self.search.read(cx).value().to_string();
                self.query.sql = None;
                self.run(window, cx);
            }
            _ => {
                self.error = true;
                self.status =
                    "Limit and Offset must be non-negative whole numbers within range".into();
                cx.notify();
            }
        }
    }

    pub(super) fn reset_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.limit
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.offset
            .update(cx, |state, cx| state.set_value("0", window, cx));
    }

    // Replace the active task and collect a bounded preview off the UI thread.
    pub(super) fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.cancellation = Cancellation::default();
        self.generation += 1;
        let generation = self.generation;

        self.busy = true;
        self.error = false;
        self.status = "Loading demo data…".into();
        self.selected = None;

        self.table.update(cx, |table, cx| {
            table.delegate_mut().loading = true;
            table.delegate_mut().preview = None;
            table.refresh(cx);
            cx.notify();
        });

        let backend = self.backend.clone();
        let query = self.query.clone();
        let cancel = self.cancellation.clone();
        let work = cx.background_executor().spawn(async move {
            let started = Instant::now();
            let result = backend
                .query(query, cancel.clone())
                .and_then(|cursor| Preview::collect(cursor, &cancel));
            (result, started.elapsed())
        });

        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let (result, elapsed) = work.await;
            let _ = this.update(cx, |this, cx| {
                // Discard results from cancelled or superseded queries.
                if generation != this.generation {
                    return;
                }

                this.busy = false;
                match result {
                    Ok(preview) => {
                        this.status = format!(
                            "{} rows{} · {:.0} ms",
                            preview.total_rows.unwrap_or(preview.rows),
                            if preview.truncated {
                                " · bounded preview"
                            } else {
                                ""
                            },
                            elapsed.as_secs_f64() * 1000.
                        );
                        this.table.update(cx, |table, cx| {
                            let grid = table.delegate_mut();
                            grid.preview = Some(preview);
                            grid.loading = false;
                            grid.sort = this.query.sort;
                            table.refresh(cx);
                        });
                    }
                    Err(error) => {
                        this.error = true;
                        this.status = error.to_string();
                        this.table.update(cx, |table, cx| {
                            table.delegate_mut().loading = false;
                            table.delegate_mut().preview = None;
                            table.refresh(cx);
                        });
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.generation += 1;
        self.busy = false;
        self.status = "Cancelled · run a query to load data".into();

        self.table.update(cx, |table, cx| {
            table.delegate_mut().loading = false;
            cx.notify();
        });
        cx.notify();
    }
}
