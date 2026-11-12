//! Browse input validation and background query lifecycle.

use super::state::{BrowseDraft, RequestKey};
use super::{TabMode, Workspace};
use gpui_kit::*;
use rowglide_app::{Cancellation, Preview};
use std::time::Instant;

impl Workspace {
    fn request_key(&self) -> RequestKey {
        RequestKey {
            generation: self.generation,
            tab: self.active_tab,
            mode: self.active_tab().mode,
        }
    }

    // Validate browse controls before replacing the active query.
    pub(super) fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().mode != TabMode::Browse {
            return;
        }
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
                self.active_tab_mut().browse = self.query.clone();
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
        let request = self.request_key();

        self.active_tab_mut().result_mut().initialized = true;
        self.busy = true;
        self.error = false;
        self.status = "Loading data…".into();
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
                if request != this.request_key() {
                    return;
                }

                this.busy = false;
                match result {
                    Ok(preview) => {
                        this.status = format!(
                            "{} loaded rows{} · {:.0} ms",
                            preview.rows,
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
        self.task = None;
        self.generation += 1;
        self.busy = false;
        self.error = false;
        self.status = "Cancelled · run a query to load data".into();

        self.table.update(cx, |table, cx| {
            table.delegate_mut().loading = false;
            cx.notify();
        });
        cx.notify();
    }

    // Save drafts separately from applied queries, including invalid input.
    pub(super) fn save_session(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.cancel(cx);
        }
        let drafts = BrowseDraft {
            search: self.search.read(cx).value().to_string(),
            limit: self.limit.read(cx).value().to_string(),
            offset: self.offset.read(cx).value().to_string(),
        };
        let sql = self.sql.read(cx).value().to_string();
        let preview = self
            .table
            .update(cx, |table, _| table.delegate_mut().preview.take());
        let selected = self.selected.take();
        let expanded = std::mem::take(&mut self.expanded);
        let status = self.status.clone();
        let error = self.error;
        let touched = self.generation;
        let tab = self.active_tab_mut();
        tab.drafts = drafts;
        tab.sql = sql;
        let result = tab.result_mut();
        result.preview = preview;
        result.selected = selected;
        result.expanded = expanded;
        result.status = status;
        result.error = error;
        result.touched = touched;
        super::state::trim_cache(&mut self.tabs);
    }

    pub(super) fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        self.query = self.active_tab().active_query();
        self.sql_open = self.active_tab().mode == TabMode::Sql;
        self.sync_controls_from_tab(window, cx);
        let result = self.active_tab_mut().result_mut();
        let initial = !result.initialized;
        let preview = result.preview.take();
        let selected = result.selected.take();
        let expanded = std::mem::take(&mut result.expanded);
        let status = result.status.clone();
        self.error = result.error;
        self.status = status;
        self.selected = selected;
        self.expanded = expanded;
        let sortable = !self.sql_open;
        let sort = if sortable { self.query.sort } else { None };
        self.table.update(cx, |table, cx| {
            let grid = table.delegate_mut();
            grid.preview = preview;
            grid.sort = sort;
            grid.sortable = sortable;
            grid.loading = false;
            table.refresh(cx);
        });
        // Only the initial file browse is automatic; SQL always requires Run.
        if initial && !self.sql_open {
            self.run(window, cx);
        }
        cx.notify();
    }

    pub(super) fn switch_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        self.save_session(cx);
        self.active_tab = index;
        self.restore_session(window, cx);
    }

    pub(super) fn switch_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_session(cx);
        let tab = self.active_tab_mut();
        tab.mode = match tab.mode {
            TabMode::Browse => TabMode::Sql,
            TabMode::Sql => TabMode::Browse,
        };
        self.restore_session(window, cx);
    }

    pub(super) fn run_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().mode != TabMode::Sql {
            return;
        }
        let sql = self.sql.read(cx).value().to_string();
        self.active_tab_mut().last_sql = Some(sql);
        self.query = self.active_tab().active_query();
        self.run(window, cx);
    }
}
