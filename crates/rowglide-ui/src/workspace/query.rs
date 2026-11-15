//! Browse input validation and background query lifecycle.

use crate::i18n::tr_string;

use super::state::{BrowseDraft, PageSummary, RequestKey};
use super::{TabMode, Workspace};
use gpui_kit::*;
use rowglide_core::{Backend, BrowsePage, Cancellation, Preview, Query, QueryError};
use std::time::Instant;

/// Validate and collect one Browse page on a background worker.
/// Propagate input, cancellation and backend errors unchanged to the UI boundary;
/// navigation metadata is created only after the complete page succeeds.
fn load_browse(
    backend: &dyn Backend,
    query: Query,
    cancel: &Cancellation,
) -> Result<(Preview, PageSummary), QueryError> {
    let range = BrowsePage::new(
        query.limit.unwrap_or(BrowsePage::default().limit()),
        query.offset,
    )?;
    let result = range.load(backend, query, cancel)?;
    let page = PageSummary {
        range,
        rows: result.preview.rows,
        has_next: result.has_next,
    };
    Ok((result.preview, page))
}

impl Workspace {
    /// Capture the generation, stable source ID and mode, or None in an empty workspace.
    ///
    /// Completion handlers compare this snapshot with the live key before updating the
    /// GUI, preventing older requests from replacing another session's result.
    pub(super) fn request_key(&self) -> Option<RequestKey> {
        let tab = self.tabs.get(self.active_tab)?;
        Some(RequestKey {
            generation: self.generation,
            source: tab.source.id,
            mode: tab.mode,
        })
    }

    /// Read the current Browse controls as owned strings without validating or applying them.
    ///
    /// Preserves unfinished/invalid text so session switches can restore exactly what
    /// the user was editing.
    pub(super) fn browse_draft(&self, cx: &App) -> BrowseDraft {
        BrowseDraft {
            filters: self.filter_drafts(cx),
            search: self.search.read(cx).value().to_string(),
            limit: self.limit.read(cx).value().to_string(),
            offset: self.offset.read(cx).value().to_string(),
        }
    }

    /// Validate and apply Browse drafts, normalize range inputs and start a new query.
    ///
    /// Preserves sorting and honors the offset unless search or predicates change. Invalid drafts only
    /// update the error/status display; they do not replace the applied query or result.
    /// Calls made in SQL mode are ignored.
    pub(super) fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().mode != TabMode::Browse || self.active_tab().changed {
            return;
        }
        let draft = self.browse_draft(cx);
        let validation = (|| -> Result<Query, QueryError> {
            let page = draft.page()?;
            let conditions = draft
                .filters
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    row.condition()
                        .map_err(|source| rowglide_core::FilterError::Condition {
                            index: index + 1,
                            source: Box::new(source),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let filters =
                rowglide_core::validate_filters(&self.active_tab().source.schema, &conditions)?;
            let search = draft.search.trim().to_owned();
            if !search.is_empty()
                && !self
                    .active_tab()
                    .source
                    .schema
                    .fields()
                    .iter()
                    .any(|field| {
                        rowglide_core::filter_kind(field.data_type())
                            == Some(rowglide_core::FilterKind::Text)
                    })
            {
                return Err(rowglide_core::FilterError::NoTextColumns.into());
            }
            let applied = &self.active_tab().browse;
            let changed = search != applied.search || filters != applied.filters;
            Ok(Query {
                search,
                filters,
                limit: Some(page.limit()),
                offset: if changed { 0 } else { page.offset() },
                ..applied.clone()
            })
        })();
        match validation {
            Ok(query) => {
                self.query = query;
                self.run(window, cx);
            }
            Err(error) => {
                self.status = crate::i18n::query_error(&error);
                self.error = true;
                cx.notify();
            }
        }
    }

    /// Check whether Previous or Next is available for the currently displayed session.
    ///
    /// `next` selects the direction. The tab checks busy state, unapplied controls and
    /// completed-page metadata; this wrapper supplies live GUI input text.
    pub(super) fn can_turn_page(&self, next: bool, cx: &App) -> bool {
        self.active_tab()
            .can_turn_page(next, self.busy, &self.browse_draft(cx))
    }

    /// Navigate one page of the applied Browse query when its controls allow it.
    ///
    /// Updates the query, saved offset draft and visible Offset input before reloading.
    /// An unavailable navigation is ignored; range overflow becomes a visible error.
    /// The current result is replaced, not appended to.
    pub(super) fn turn_page(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_turn_page(next, cx) {
            return;
        }
        let range = self.browse_draft(cx).page().and_then(|page| {
            if next {
                page.next_page()
            } else {
                Ok(page.previous())
            }
        });
        match range {
            Ok(page) => {
                self.query.offset = page.offset();

                self.active_tab_mut().drafts.offset = page.offset().to_string();
                self.offset.update(cx, |state, cx| {
                    state.set_value(page.offset().to_string(), window, cx)
                });
                self.run(window, cx);
            }
            Err(error) => {
                self.error = true;
                self.status = crate::i18n::query_error(&error);
                cx.notify();
            }
        }
    }

    /// Restore empty filter text, the default page size and offset zero in the controls.
    ///
    /// This updates GUI text only. Reset prepares a candidate and saved drafts;
    /// applied state changes only after successful execution.
    pub(super) fn reset_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.limit.update(cx, |state, cx| {
            state.set_value(BrowsePage::default().limit().to_string(), window, cx)
        });
        self.offset
            .update(cx, |state, cx| state.set_value("0", window, cx));
    }

    /// Replace the active request and collect its result away from the GUI thread.
    ///
    /// Cancels the preceding token and advances request identity. Browse retains its
    /// applied page while collecting a complete bounded replacement; SQL clears its
    /// previous session and starts a continuously generated cached result. Completion updates status
    /// only if tab, mode and generation still match, resets scrolling for nonempty
    /// results, and reports errors without publishing partial Browse pages. Changed or
    /// reloading sources are rejected before altering the current result.
    pub(super) fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().changed || self.reloading == Some(self.active_tab().source.id) {
            cx.notify();
            return;
        }
        if !self.sql_open {
            self.run_browse(window, cx);
            return;
        }
        self.cancellation.cancel();
        self.task = None;
        self.cancellation = Cancellation::default();
        self.generation += 1;

        self.sizing_task = None;
        if self.sql_open {
            self.active_tab_mut().result_mut().widths = None;
        }
        let widths = self.active_tab_mut().result_mut().widths.clone();
        self.active_tab_mut().result_mut().initialized = true;
        self.active_tab_mut().result_mut().page = None;
        self.busy = true;
        self.error = false;
        self.status = tr_string!("query.loading");
        self.selected = None;
        self.pending_cell = None;
        self.expanded.clear();

        self.table.update(cx, |table, cx| {
            table.clear_selection(cx);
            table.delegate_mut().widths = widths;
            table.delegate_mut().loading = true;
            table.delegate_mut().preview = None;
            table.delegate_mut().sql_result = None;
            table.delegate_mut().sql_snapshot = None;
            table.refresh(cx);
            cx.notify();
        });

        if let Err(error) = self.start_sql(cx) {
            self.finish_query_error(error, cx);
        }
        cx.notify();
    }

    /// Execute a candidate Browse query while retaining the last successful page and selection.
    /// Only a complete current response commits applied state. Unchanged editor drafts are
    /// normalized on success; text edited while work runs remains untouched.
    fn run_browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.task = None;
        self.cancellation = Cancellation::default();
        self.generation += 1;
        let key = self.request_key();
        let candidate = self.query.clone();
        let draft = self.browse_draft(cx);
        let ranges_match = draft.page().is_ok_and(|page| {
            Some(page.limit()) == candidate.limit && page.offset() == candidate.offset
        });
        let predicates_changed = candidate.search != self.active_tab().browse.search
            || candidate.filters != self.active_tab().browse.filters;
        let normalize = (ranges_match
            || predicates_changed
            || draft.matches_query(&self.active_tab().browse))
            && draft.search.trim() == candidate.search
            && draft
                .filters
                .iter()
                .map(super::state::FilterDraft::condition)
                .collect::<Result<Vec<_>, _>>()
                .and_then(|conditions| {
                    rowglide_core::validate_filters(&self.active_tab().source.schema, &conditions)
                })
                .is_ok_and(|conditions| conditions == candidate.filters);
        self.active_tab_mut().browse_result.initialized = true;
        self.busy = true;
        self.error = false;
        self.status = tr_string!("query.loading");
        self.table.update(cx, |table, cx| {
            table.delegate_mut().loading = !table.delegate().has_result();
            cx.notify();
        });
        let backend = self.backend.clone();
        let cancel = self.cancellation.clone();
        let query = candidate.clone();
        let work = cx.background_executor().spawn(async move {
            let started = Instant::now();
            (
                load_browse(backend.as_ref(), query, &cancel),
                started.elapsed(),
            )
        });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let (result, elapsed) = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if key != this.request_key() {
                    return;
                }
                this.busy = false;
                match result {
                    Ok((preview, page)) => {
                        if normalize && this.browse_draft(cx) == draft {
                            let tab = this.active_tab_mut();
                            tab.drafts.search = candidate.search.clone();
                            tab.drafts.limit = candidate
                                .limit
                                .unwrap_or(BrowsePage::default().limit())
                                .to_string();
                            tab.drafts.offset = candidate.offset.to_string();
                            for (row, condition) in
                                tab.drafts.filters.iter_mut().zip(&candidate.filters)
                            {
                                if let rowglide_core::FilterCondition::Compare { value, .. } =
                                    condition
                                {
                                    row.value = value.input();
                                }
                            }
                            this.sync_controls_from_tab(window, cx);
                        }
                        this.active_tab_mut().browse = candidate.clone();
                        this.active_tab_mut().browse_result.page = Some(page);
                        this.status =
                            format!("{} · {:.0} ms", page.label(), elapsed.as_secs_f64() * 1000.);
                        this.selected = None;
                        this.pending_cell = None;
                        this.expanded.clear();
                        let widths = this.active_tab().browse_result.widths.clone();
                        let empty_label = this.browse_empty_label(&candidate);
                        this.table.update(cx, |table, cx| {
                            table.clear_selection(cx);
                            let grid = table.delegate_mut();
                            grid.empty_label = empty_label;
                            grid.preview = Some(preview);
                            grid.sql_result = None;
                            grid.sql_snapshot = None;
                            grid.loading = false;
                            grid.sort = candidate.sort;
                            grid.widths = widths;
                            table.refresh(cx);
                            table.scroll_to_row(0, cx);
                        });
                        this.size_columns(cx);
                    }
                    Err(error) => {
                        this.query = this.active_tab().browse.clone();
                        this.finish_query_error(error, cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Handle Browse failure or SQL startup failure at the GUI boundary.
    /// Browse retains its previous successful result; stop loading and show the cause.
    /// Cancellation is a normal terminal state and does not use the error color.
    /// Streamed SQL failures use snapshots instead so committed rows remain available.
    fn finish_query_error(&mut self, error: QueryError, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = !matches!(error, QueryError::Cancelled);
        self.status = crate::i18n::query_error(&error);
        if !self.sql_open && self.table.read(cx).delegate().has_result() {
            self.status = tr_string!("filters.previous", error = self.status);
        }
        self.table.update(cx, |table, cx| {
            table.delegate_mut().loading = false;
            table.refresh(cx);
        });
        cx.notify();
    }

    /// Signal query cancellation, release the UI task handle and invalidate late results.
    ///
    /// Clears the busy/loading flags and displays cancellation status. Browse retains
    /// its previous result/page metadata; SQL retains already committed batches. This does not join DuckDB on
    /// the GUI thread; background cursor cleanup releases engine resources.
    pub(super) fn cancel(&mut self, cx: &mut Context<Self>) {
        self.sizing_task = None;
        self.cancellation.cancel();
        self.task = None;
        self.generation += 1;
        self.busy = false;
        self.error = false;
        self.status = tr_string!("query.cancelled");
        if !self.sql_open && !self.tabs.is_empty() {
            self.query = self.active_tab().browse.clone();
            if self.table.read(cx).delegate().has_result() {
                self.status = tr_string!("filters.previous", error = self.status);
            }
        }
        if let Some(result) = &self.table.read(cx).delegate().sql_result {
            result.cancel();
        }

        self.table.update(cx, |table, cx| {
            table.delegate_mut().loading = false;
            cx.notify();
        });
        self.watch_sql(cx);
        cx.notify();
    }

    /// Save live inputs and move the active result/inspector into the current tab mode.
    ///
    /// Cancels pending work first, preserves invalid drafts independently of applied
    /// queries, records the cache recency stamp and trims inactive results. Arrow
    /// buffers move out of the shared table rather than being copied.
    pub(super) fn save_session(&mut self, cx: &mut Context<Self>) {
        self.sizing_task = None;
        let widths = self.table.read(cx).delegate().widths.clone();
        self.pending_cell = None;
        if self.busy {
            self.cancel(cx);
        }
        self.task = None;
        let drafts = self.browse_draft(cx);
        let sql = self.sql.read(cx).value().to_string();
        let preview = self
            .table
            .update(cx, |table, _| table.delegate_mut().preview.take());
        let sql_result = self.table.update(cx, |table, _| {
            table.delegate_mut().sql_snapshot = None;
            table.delegate_mut().sql_result.take()
        });
        let selected = self.selected.take();
        let expanded = std::mem::take(&mut self.expanded);
        let status = self.status.clone();
        let error = self.error;
        let touched = self.generation;
        let tab = self.active_tab_mut();
        tab.drafts = drafts;
        tab.sql = sql;
        let result = tab.result_mut();
        result.widths = widths;
        result.preview = preview;
        result.sql_result = sql_result;
        result.selected = selected;
        result.expanded = expanded;
        result.status = status;
        result.error = error;
        result.touched = touched;
        super::state::trim_cache(&mut self.tabs);
    }

    /// Restore the selected tab/mode's applied query, drafts, result and inspector.
    ///
    /// Moves retained data back into the shared table and updates sorting availability.
    /// Only a never-initialized Browse session starts automatically; SQL and evicted
    /// results require explicit execution. Callers save/cancel the previous session
    /// before changing the active tab or mode.
    pub(super) fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        self.query = self.active_tab().active_query();
        self.sql_open = self.active_tab().mode == TabMode::Sql;
        self.sync_controls_from_tab(window, cx);
        let result = self.active_tab_mut().result_mut();
        let initial = !result.initialized;
        let widths = result.widths.clone();
        let preview = result.preview.take();
        let sql_result = result.sql_result.take();
        let selected = result.selected.take();
        let expanded = std::mem::take(&mut result.expanded);
        let status = result.status.clone();
        self.error = result.error;
        self.status = status;
        self.selected = selected;
        self.expanded = expanded;
        let sortable = !self.sql_open;
        let sort = if sortable { self.query.sort } else { None };
        let empty_label = self.browse_empty_label(&self.query);
        self.table.update(cx, |table, cx| {
            let grid = table.delegate_mut();
            grid.widths = widths;
            grid.empty_label = empty_label;
            grid.preview = preview;
            grid.sql_snapshot = sql_result.as_ref().map(|result| result.snapshot());
            grid.sql_result = sql_result;
            grid.sort = sort;
            grid.sortable = sortable;
            grid.loading = false;
            table.refresh(cx);
        });
        self.watch_sql(cx);
        // Only the initial file browse is automatic; SQL always requires Run.
        if initial && !self.sql_open {
            self.run(window, cx);
        }
        self.size_columns(cx);
        cx.notify();
    }

    /// Save the current session and restore another valid file tab.
    ///
    /// Out-of-range indices and clicks on the already active tab are ignored. Saving
    /// cancels pending work so its completion cannot overwrite the restored tab.
    pub(super) fn switch_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        self.save_session(cx);
        self.active_tab = index;
        self.restore_session(window, cx);
        self.refresh_file_info(cx);
    }

    /// Save the active mode, toggle Browse/SQL and restore the other mode's session.
    ///
    /// Browse and SQL retain separate results and applied queries; entering SQL restores
    /// its draft without executing it.
    pub(super) fn switch_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_session(cx);
        let tab = self.active_tab_mut();
        tab.mode = match tab.mode {
            TabMode::Browse => TabMode::Sql,
            TabMode::Sql => TabMode::Browse,
        };
        self.restore_session(window, cx);
    }

    /// Snapshot the SQL editor text as the last executed statement and start a request.
    ///
    /// Only operates in SQL mode. The query is rebuilt from the tab's SQL state so
    /// Browse filtering, sorting and range settings are excluded.
    pub(super) fn run_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().mode != TabMode::Sql || self.active_tab().changed {
            return;
        }
        let sql = self.sql.read(cx).value().to_string();
        self.active_tab_mut().last_sql = Some(sql);
        self.query = self.active_tab().active_query();
        self.run(window, cx);
    }
}
